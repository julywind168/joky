//! Byte offsets guarded by enclosing enum tags, ordered from outermost to innermost.
use super::*;
use crate::core::types::memory_layout::MemoryLayoutShape;

#[derive(Clone)]
pub(super) struct Owner {
    pub offset: usize,
    guards: Vec<(usize, i32)>,
}
impl Owner {
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.guards.len() * std::mem::size_of::<(usize, i32)>()
    }
    /// Test outer tags before inspecting nested variants.
    /// # Safety
    /// `root` must address this descriptor's live, initialized layout. Tags in
    /// active variants must be readable Int32 values, with no concurrent writes.
    pub unsafe fn active(&self, root: *const u8) -> bool {
        self.guards
            .iter()
            .all(|&(offset, tag)| root.add(offset).cast::<i32>().read_unaligned() == tag)
    }
    pub fn describe(ty: &Type) -> Result<Vec<Self>, Error> {
        fn visit(
            ty: &Type,
            base: usize,
            guards: &mut Vec<(usize, i32)>,
            out: &mut Vec<Owner>,
        ) -> Result<(), Error> {
            if !ty.is_owner() {
                return Ok(());
            }
            if ty.is_owner_lane() {
                out.push(Owner {
                    offset: base,
                    guards: guards.clone(),
                });
                return Ok(());
            }
            let layout = ty.storage_layout().map_err(|e| invalid(&e.to_string()))?;
            match (ty, layout.shape) {
                (Type::Record(r), MemoryLayoutShape::Record(fields)) => {
                    for (field, location) in r.fields.iter().zip(fields) {
                        visit(&field.ty, base + location.offset, guards, out)?;
                    }
                }
                (Type::Tuple(t), MemoryLayoutShape::Tuple(fields)) => {
                    for (ty, location) in t.elements.iter().zip(fields) {
                        visit(ty, base + location.offset, guards, out)?;
                    }
                }
                (Type::Enum(e), MemoryLayoutShape::Enum { tag, variants }) => {
                    for (index, (variant, location)) in e.variants.iter().zip(variants).enumerate()
                    {
                        if let (Some(ty), Some(location)) = (&variant.payload, location.payload) {
                            guards.push((base + tag.offset, index as i32));
                            visit(ty, base + location.offset, guards, out)?;
                            guards.pop();
                        }
                    }
                }
                _ => return Err(invalid("Unsupported domain owner layout")),
            }
            Ok(())
        }
        let mut out = Vec::new();
        visit(ty, 0, &mut Vec::new(), &mut out)?;
        Ok(out)
    }
}
