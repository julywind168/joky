//! Compiler-derived old views; never infer a layout from user-written declarations.
use super::*;

pub(super) struct View {
    pub imports: String,
    pub declarations: String,
    types: Vec<(Type, String)>,
    left: usize,
    arrays: String,
    maps: String,
    add_arrays: bool,
    add_maps: bool,
}
impl View {
    pub fn new(ty: &Type, imports: &[crate::parser::ast::Import]) -> Result<Self, Error> {
        let alias = |path: &str| {
            imports.iter().find(|i| i.path == path).map(|i| {
                i.alias
                    .clone()
                    .unwrap_or_else(|| path.rsplit('/').next().unwrap().to_string())
            })
        };
        let arrays = alias("joky/array");
        let maps = alias("joky/map");
        let mut view = Self {
            imports: String::new(),
            add_arrays: arrays.is_none(),
            add_maps: maps.is_none(),
            arrays: arrays.unwrap_or_else(|| "PreviousArrays".into()),
            maps: maps.unwrap_or_else(|| "PreviousMaps".into()),
            declarations: String::new(),
            types: Vec::new(),
            left: 65_536,
        };
        if !matches!(ty, Type::Record(_)) {
            return Err(invalid("Migration root must be a record"));
        }
        view.render(ty, 0)?;
        Ok(view)
    }
    fn name(&self, nominal: &str) -> String {
        let base = format!("Previous{}", nominal.rsplit('.').next().unwrap());
        if base != "PreviousState" && !self.types.iter().any(|(_, name)| *name == base) {
            base
        } else {
            let mut index = self.types.len();
            loop {
                let name = format!("PreviousType{index}");
                if !self.types.iter().any(|(_, used)| *used == name) {
                    break name;
                }
                index += 1;
            }
        }
    }
    fn render(&mut self, ty: &Type, depth: usize) -> Result<String, Error> {
        if depth > 32 || self.left == 0 {
            return Err(invalid("Migration view exceeds finite storage limits"));
        }
        self.left -= 1;
        if instance::scalar(ty) || *ty == Type::String {
            return Ok(ty.to_string());
        }
        if let Some((_, name)) = self.types.iter().find(|(t, _)| t == ty) {
            return Ok(name.clone());
        }
        match ty {
            Type::Tuple(t) => {
                let names = t
                    .elements
                    .iter()
                    .map(|t| self.render(t, depth + 1))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(format!("({},)", names.join(",")))
            }
            Type::Record(r) if r.name == "joky/array.Array" || r.name == "joky/map.Map" => {
                let container = if r.name == "joky/array.Array" {
                    if self.add_arrays {
                        self.imports
                            .push_str("import joky/array as PreviousArrays\n");
                        self.add_arrays = false;
                    }
                    format!("{}.Array", self.arrays)
                } else {
                    if self.add_maps {
                        self.imports.push_str("import joky/map as PreviousMaps\n");
                        self.add_maps = false;
                    }
                    format!("{}.Map", self.maps)
                };
                let arguments = r
                    .arguments
                    .iter()
                    .map(|t| self.render(t, depth + 1))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(format!("{container}<{}>", arguments.join(",")))
            }
            Type::Record(r) if !r.indirect && r.closure.is_none() => {
                let name = if self.types.is_empty() {
                    "PreviousState".to_string()
                } else {
                    self.name(&r.name)
                };
                self.types.push((ty.clone(), name.clone()));
                let mut declaration = if r.repr_c {
                    format!("\n#repr(C)\nstruct {name} {{\n")
                } else {
                    format!("\nstruct {name} {{\n")
                };
                for field in &r.fields {
                    declaration.push_str(&format!(
                        "{}: {},\n",
                        field.name,
                        self.render(&field.ty, depth + 1)?
                    ));
                }
                declaration.push_str("}\n");
                self.declarations.push_str(&declaration);
                Ok(name)
            }
            Type::Enum(e) if !e.indirect && !ty.is_owner() => {
                let name = self.name(&e.name);
                self.types.push((ty.clone(), name.clone()));
                let mut declaration = format!("\nenum {name} {{\n");
                for variant in &e.variants {
                    declaration.push_str(&variant.name);
                    if let Some(payload) = &variant.payload {
                        declaration.push_str(&format!("({})", self.render(payload, depth + 1)?));
                    }
                    declaration.push_str(",\n");
                }
                declaration.push_str("}\n");
                self.declarations.push_str(&declaration);
                Ok(name)
            }
            _ => Err(invalid("Unsupported storage in migration old view")),
        }
    }
}

/// Names/ids intentionally differ. Compare every reachable layout and field,
/// including buffer elements; nominal equality alone could hide a changed graph.
pub(super) fn matches(old: &Type, view: &Type) -> bool {
    fn check(a: &Type, b: &Type, depth: usize, left: &mut usize) -> bool {
        if depth > 32 || *left == 0 {
            return false;
        }
        *left -= 1;
        if a.storage_layout().ok() != b.storage_layout().ok() {
            return false;
        }
        let mut child = |a, b| check(a, b, depth + 1, left);
        match (a, b) {
            (Type::Buffer(a), Type::Buffer(b)) => child(a, b),
            (Type::Record(a), Type::Record(b)) => {
                !a.indirect
                    && !b.indirect
                    && a.repr_c == b.repr_c
                    && a.fields.len() == b.fields.len()
                    && a.fields.iter().zip(&b.fields).all(|(a, b)| {
                        a.name == b.name && a.offset == b.offset && child(&a.ty, &b.ty)
                    })
            }
            (Type::Tuple(a), Type::Tuple(b)) => {
                a.elements.len() == b.elements.len()
                    && a.elements.iter().zip(&b.elements).all(|(a, b)| child(a, b))
            }
            (Type::Enum(a), Type::Enum(b)) => {
                !a.indirect
                    && !b.indirect
                    && !Type::Enum(a.clone()).is_owner()
                    && !Type::Enum(b.clone()).is_owner()
                    && a.variants.len() == b.variants.len()
                    && a.variants.iter().zip(&b.variants).all(|(a, b)| {
                        a.name == b.name
                            && a.offset == b.offset
                            && match (&a.payload, &b.payload) {
                                (Some(a), Some(b)) => child(a, b),
                                (None, None) => true,
                                _ => false,
                            }
                    })
            }
            _ => (instance::scalar(a) || *a == Type::String) && a == b,
        }
    }
    matches!(view,Type::Record(r) if r.name=="PreviousState") && check(old, view, 0, &mut 65_536)
}
