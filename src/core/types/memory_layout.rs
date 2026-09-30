//! Provisional object byte layouts, independent of scalar slots and call ABIs.
//!
//! These descriptions do not allocate storage, change existing lowering, or
//! establish C compatibility. Query resolved types after alias normalization and
//! specialization; a stored type parameter has no concrete byte layout yet.
use crate::parser::ast::{EnumType, RecordType, TupleType, Type};
use std::{collections::HashMap, sync::Arc};

const MAX_MEMBERS: usize = 65_536;
const MAX_DEPTH: usize = 256;

/// Target input. Default uses the native integer/pointer alignments and size
/// limit. Explicit cross-target inputs do not establish a foreign call ABI.
///
/// Integer widths stay fixed at 4/8 bytes; their ABI alignments may differ
/// between targets. Internal Bool is one byte (0/1), Unit is zero bytes with
/// alignment 1. Reference layout inputs do not establish C compatibility, byte order, or a call ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryLayoutTarget {
    pub int32_align: usize,
    pub int64_align: usize,
    /// Size and alignment of references and opaque Ptr values, independent of integer lanes.
    pub pointer_size: usize,
    pub pointer_align: usize,
    /// Maximum padded object size, bounded by the host's `isize::MAX`.
    /// A cross compiler must supply the destination's smaller limit if needed.
    pub max_size: usize,
}

impl Default for MemoryLayoutTarget {
    fn default() -> Self {
        Self {
            int32_align: std::mem::align_of::<i32>(),
            int64_align: std::mem::align_of::<i64>(),
            pointer_size: std::mem::size_of::<usize>(),
            pointer_align: std::mem::align_of::<usize>(),
            max_size: isize::MAX as usize,
        }
    }
}

impl MemoryLayoutTarget {
    fn validate(self) -> Result<(), MemoryLayoutError> {
        for align in [self.int32_align, self.int64_align, self.pointer_align] {
            if !align.is_power_of_two() {
                return Err(MemoryLayoutError::InvalidTarget);
            }
        }
        if !matches!(self.pointer_size, 4 | 8) || self.max_size > isize::MAX as usize {
            return Err(MemoryLayoutError::InvalidTarget);
        }
        Ok(())
    }
}

/// Why a byte layout could not be computed. No scalar-slot fallback is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryLayoutError {
    /// String, opaque/unresolved types, applications and stored type parameters.
    UnsupportedType,
    RecursiveType,
    Overflow,
    /// Object size, member count or nesting exceeds the supported limit.
    ResourceLimit,
    InvalidTarget,
}

impl std::fmt::Display for MemoryLayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnsupportedType => "Type has no concrete addressable memory layout",
            Self::RecursiveType => "Recursive type has no finite addressable memory layout",
            Self::Overflow => "Memory layout arithmetic overflow",
            Self::ResourceLimit => "Memory layout exceeds size, member or nesting limit",
            Self::InvalidTarget => "Invalid memory layout target",
        })
    }
}
impl std::error::Error for MemoryLayoutError {}

/// Padded size, alignment and immediate member locations of one object.
/// Padding is not a value and must not participate in equality or serialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryLayout {
    pub size: usize,
    pub align: usize,
    pub shape: MemoryLayoutShape,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryLayoutShape {
    Scalar,
    /// Members in declaration order; offsets are relative to this object.
    Record(Vec<FieldMemoryLayout>),
    Tuple(Vec<FieldMemoryLayout>),
    /// Int32 tag and disjoint payload regions, not a C union representation.
    Enum {
        tag: FieldMemoryLayout,
        variants: Vec<VariantMemoryLayout>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldMemoryLayout {
    pub offset: usize,
    pub size: usize,
    pub align: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VariantMemoryLayout {
    /// None for a nullary variant; Some(size = 0) for an explicit Unit payload.
    pub payload: Option<FieldMemoryLayout>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum AggregateKey {
    Record(*const RecordType),
    Enum(*const EnumType),
    Tuple(*const TupleType),
}

impl Type {
    /// Describe an object's byte layout using the default target assumptions.
    pub fn memory_layout(&self) -> Result<MemoryLayout, MemoryLayoutError> {
        self.memory_layout_for(MemoryLayoutTarget::default())
    }

    /// Describe an object's byte layout for an explicit target.
    ///
    /// This is a provisional introspection API. It does not change scalar-slot
    /// offsets, materialize objects, or determine parameter/return conventions.
    pub fn memory_layout_for(
        &self,
        target: MemoryLayoutTarget,
    ) -> Result<MemoryLayout, MemoryLayoutError> {
        target.validate()?;
        let mut context = LayoutContext {
            target,
            cache: HashMap::new(),
            visiting: std::collections::HashSet::new(),
        };
        context.layout(self, 0).map(|layout| (*layout).clone())
    }
}

struct LayoutContext {
    target: MemoryLayoutTarget,
    // Scoped to one query: source Arcs stay alive, and target-dependent results
    // cannot leak between targets. Shared zero-sized DAGs are visited once;
    // Arc avoids copying a child's entire field list for each incoming edge.
    cache: HashMap<AggregateKey, (Arc<MemoryLayout>, usize)>,
    visiting: std::collections::HashSet<AggregateKey>,
}

impl LayoutContext {
    fn layout(&mut self, ty: &Type, depth: usize) -> Result<Arc<MemoryLayout>, MemoryLayoutError> {
        let (key, count) = match ty {
            Type::Record(r) if r.indirect => return Err(MemoryLayoutError::UnsupportedType),
            Type::Enum(e) if e.indirect => return Err(MemoryLayoutError::UnsupportedType),
            Type::Record(r) => (AggregateKey::Record(Arc::as_ptr(r)), r.fields.len()),
            Type::Enum(e) => (AggregateKey::Enum(Arc::as_ptr(e)), e.variants.len()),
            Type::Tuple(t) => (AggregateKey::Tuple(Arc::as_ptr(t)), t.elements.len()),
            _ => {
                let (size, align) = match ty {
                    Type::Int32 | Type::UInt32 => (4, self.target.int32_align),
                    Type::Int64 | Type::UInt64 => (8, self.target.int64_align),
                    Type::Bool => (1, 1),
                    Type::Ptr => (self.target.pointer_size, self.target.pointer_align),
                    Type::Reference(r) if !r.mutable && !r.target.is_buffer() => {
                        (self.target.pointer_size, self.target.pointer_align)
                    }
                    Type::Unit => (0, 1),
                    _ => return Err(MemoryLayoutError::UnsupportedType),
                };
                if size > self.target.max_size {
                    return Err(MemoryLayoutError::ResourceLimit);
                }
                return Ok(Arc::new(MemoryLayout {
                    size,
                    align,
                    shape: MemoryLayoutShape::Scalar,
                }));
            }
        };
        // Check before allocating member vectors or descending. Raw tuple ASTs
        // can be deeper than the resolver's already-bounded canonical types.
        if depth >= MAX_DEPTH || count > MAX_MEMBERS {
            return Err(MemoryLayoutError::ResourceLimit);
        }
        if let Some((layout, height)) = self.cache.get(&key) {
            if depth + height > MAX_DEPTH {
                return Err(MemoryLayoutError::ResourceLimit);
            }
            return Ok(layout.clone());
        }
        if !self.visiting.insert(key) {
            return Err(MemoryLayoutError::RecursiveType);
        }
        let tag_align = self.target.int32_align;
        let mut builder = AggregateBuilder::new(self.target.max_size);
        let mut height = 1;
        let mut member = |ty: &Type, builder: &mut AggregateBuilder| {
            let layout = self.layout(ty, depth + 1)?;
            let child_key = match ty {
                Type::Record(r) => Some(AggregateKey::Record(Arc::as_ptr(r))),
                Type::Enum(e) => Some(AggregateKey::Enum(Arc::as_ptr(e))),
                Type::Tuple(t) => Some(AggregateKey::Tuple(Arc::as_ptr(t))),
                _ => None,
            };
            if let Some(key) = child_key {
                height = height.max(self.cache[&key].1 + 1);
            }
            builder.push(layout.size, layout.align)
        };
        let shape = match ty {
            Type::Record(record) => MemoryLayoutShape::Record(
                record
                    .fields
                    .iter()
                    .map(|f| member(&f.ty, &mut builder))
                    .collect::<Result<_, _>>()?,
            ),
            Type::Tuple(tuple) => MemoryLayoutShape::Tuple(
                tuple
                    .elements
                    .iter()
                    .map(|ty| member(ty, &mut builder))
                    .collect::<Result<_, _>>()?,
            ),
            Type::Enum(enumeration) => {
                let tag = builder.push(4, tag_align)?;
                let variants = enumeration
                    .variants
                    .iter()
                    .map(|variant| {
                        let payload = variant
                            .payload
                            .as_ref()
                            .map(|ty| member(ty, &mut builder))
                            .transpose()?;
                        Ok(VariantMemoryLayout { payload })
                    })
                    .collect::<Result<_, MemoryLayoutError>>()?;
                MemoryLayoutShape::Enum { tag, variants }
            }
            _ => unreachable!(),
        };
        let layout = builder.finish(shape);
        self.visiting.remove(&key);
        let layout = Arc::new(layout?);
        self.cache.insert(key, (layout.clone(), height));
        Ok(layout)
    }
}

struct AggregateBuilder {
    size: usize,
    align: usize,
    max_size: usize,
}

impl AggregateBuilder {
    fn new(max_size: usize) -> Self {
        Self {
            size: 0,
            align: 1,
            max_size,
        }
    }

    fn push(&mut self, size: usize, align: usize) -> Result<FieldMemoryLayout, MemoryLayoutError> {
        let offset = checked_align_up(self.size, align, self.max_size)?;
        let end = offset
            .checked_add(size)
            .ok_or(MemoryLayoutError::Overflow)?;
        if end > self.max_size {
            return Err(MemoryLayoutError::ResourceLimit);
        }
        self.size = end;
        self.align = self.align.max(align);
        Ok(FieldMemoryLayout {
            offset,
            size,
            align,
        })
    }

    fn finish(self, shape: MemoryLayoutShape) -> Result<MemoryLayout, MemoryLayoutError> {
        Ok(MemoryLayout {
            size: checked_align_up(self.size, self.align, self.max_size)?,
            align: self.align,
            shape,
        })
    }
}

fn checked_align_up(
    value: usize,
    align: usize,
    max_size: usize,
) -> Result<usize, MemoryLayoutError> {
    let mask = align - 1; // all alignments are validated at the query boundary
    let aligned = value.checked_add(mask).ok_or(MemoryLayoutError::Overflow)? & !mask;
    if aligned > max_size {
        return Err(MemoryLayoutError::ResourceLimit);
    }
    Ok(aligned)
}

#[cfg(test)]
mod tests;

impl Type {
    /// Scalar locations in declaration order, using object bytes rather than IR slots.
    pub(crate) fn memory_scalars(&self) -> Result<Vec<(usize, Type)>, MemoryLayoutError> {
        fn visit(
            context: &mut LayoutContext,
            ty: &Type,
            base: usize,
            out: &mut Vec<(usize, Type)>,
        ) -> Result<(), MemoryLayoutError> {
            let layout = context.layout(ty, 0)?;
            // Zero-sized DAGs must not be recursively expanded.
            if ty.scalar_types().is_empty() {
                return Ok(());
            }
            match (ty, &layout.shape) {
                (Type::Record(record), MemoryLayoutShape::Record(fields)) => {
                    for (field, location) in record.fields.iter().zip(fields) {
                        visit(context, &field.ty, base + location.offset, out)?;
                    }
                }
                (Type::Tuple(tuple), MemoryLayoutShape::Tuple(fields)) => {
                    for (element, location) in tuple.elements.iter().zip(fields) {
                        visit(context, element, base + location.offset, out)?;
                    }
                }
                (Type::Enum(enumeration), MemoryLayoutShape::Enum { tag, variants }) => {
                    out.push((base + tag.offset, Type::Int32));
                    for (variant, location) in enumeration.variants.iter().zip(variants) {
                        if let (Some(ty), Some(location)) = (&variant.payload, &location.payload) {
                            visit(context, ty, base + location.offset, out)?;
                        }
                    }
                }
                _ => out.push((base, ty.clone())),
            }
            Ok(())
        }
        let mut context = LayoutContext {
            target: MemoryLayoutTarget::default(),
            cache: HashMap::new(),
            visiting: std::collections::HashSet::new(),
        };
        let mut result = Vec::new();
        visit(&mut context, self, 0, &mut result)?;
        Ok(result)
    }
    pub(crate) fn memory_field_path(
        &self,
        path: &[String],
    ) -> Result<(usize, Type), MemoryLayoutError> {
        let mut target = self.clone();
        let mut offset = 0;
        for name in path {
            let (Type::Record(record), MemoryLayoutShape::Record(fields)) =
                (&target, target.memory_layout()?.shape)
            else {
                return Err(MemoryLayoutError::UnsupportedType);
            };
            let index = record
                .fields
                .iter()
                .position(|f| &f.name == name)
                .ok_or(MemoryLayoutError::UnsupportedType)?;
            offset += fields[index].offset;
            target = record.fields[index].ty.clone();
        }
        Ok((offset, target))
    }
}
