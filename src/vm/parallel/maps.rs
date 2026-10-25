//! Sealed host snapshots retain language-produced indices without invoking key protocols.
use super::{arrays, invalid, TaskValue};
use crate::{
    runtime::{buffer::Buffer, parallel::reserve_current, value_storage::read_memory},
    Error, Type,
};

/// A read-only, pointer-free Map snapshot produced by a checked task.
///
/// Clone or return it as `TaskValue::Map` to preserve the mapping. Create new maps
/// by passing an Array of entries to a language factory using `Map.from_entries`;
/// only language code can run the key's exact Hash/Eq implementations. Equality
/// compares the snapshot representation, not language key equality.
#[derive(Clone, Debug, PartialEq)]
pub struct TaskMap {
    pub(super) ty: Type,
    pub(super) fields: Vec<TaskValue>,
}
impl TaskMap {
    /// The exact nominal Map type from the frozen signature.
    pub fn value_type(&self) -> &Type {
        &self.ty
    }
    /// Keys in unspecified dense storage order. No mutable key view is exposed.
    pub fn keys(&self) -> &[TaskValue] {
        elements(&self.fields[1])
    }
    /// Values paired positionally with `keys()`.
    pub fn values(&self) -> &[TaskValue] {
        elements(&self.fields[2])
    }
    /// Entries in unspecified order, with each key paired with its value.
    pub fn entries(&self) -> impl ExactSizeIterator<Item = (&TaskValue, &TaskValue)> {
        self.keys().iter().zip(self.values())
    }
    /// Number of resident entries.
    pub fn len(&self) -> usize {
        self.keys().len()
    }
    /// Whether the snapshot has no resident entries.
    pub fn is_empty(&self) -> bool {
        self.keys().is_empty()
    }
}

/// A read-only, pointer-free Set snapshot produced by a checked task.
///
/// Clone or return it as `TaskValue::Set`. New host elements must pass through a
/// language factory using `Set.from_array`, so hashing and deduplication remain
/// subject to the same call-graph checks as other worker code. Equality compares
/// the snapshot representation, not mathematical set equality.
#[derive(Clone, Debug, PartialEq)]
pub struct TaskSet {
    pub(super) ty: Type,
    pub(super) fields: Vec<TaskValue>,
}
impl TaskSet {
    /// The exact nominal Set type from the frozen signature.
    pub fn value_type(&self) -> &Type {
        &self.ty
    }
    /// Elements in unspecified order, without mutable access to indexed keys.
    pub fn elements(&self) -> &[TaskValue] {
        let TaskValue::Map(map) = &self.fields[0] else {
            unreachable!()
        };
        map.keys()
    }
    /// Number of resident entries.
    pub fn len(&self) -> usize {
        self.elements().len()
    }
    /// Whether the snapshot has no resident entries.
    pub fn is_empty(&self) -> bool {
        self.elements().is_empty()
    }
}
fn elements(value: &TaskValue) -> &[TaskValue] {
    let TaskValue::Array { elements, .. } = value else {
        unreachable!()
    };
    elements
}

/// Validate the trusted Map's structure before sealing a host snapshot. Cached
/// hashes are preserved, never recomputed using a potentially different protocol.
/// # Safety
/// `words` is a live Map of the exact compiler-provided type, exclusively retained
/// by a checked packet. All five buffers and their initialized fields remain live.
pub(super) unsafe fn validate(ty: &Type, words: &[u64]) -> Result<(), Error> {
    let Type::Record(r) = ty else {
        return Err(invalid("Expected Map type"));
    };
    if !r.map {
        return Err(invalid("Expected Map type"));
    }
    let fields = &r.fields;
    let mut counts = [0; 5];
    for (count, field) in counts.iter_mut().zip(fields) {
        *count = arrays::length(words[field.offset])?;
    }
    let [count, length, values, hashes, positions] = counts;
    let deleted = words[fields[5].offset];
    if length != values || length != hashes || length != positions {
        return Err(invalid("Inconsistent Map column lengths"));
    }
    if count == 0 {
        return if length == 0 && deleted == 0 {
            Ok(())
        } else {
            Err(invalid("Invalid empty Map"))
        };
    }
    if count < 8
        || !count.is_power_of_two()
        || (length as u64)
            .checked_add(deleted)
            .is_none_or(|n| n > (count - count / 4) as u64)
    {
        return Err(invalid("Invalid Map bucket count or load"));
    }
    let bucket = fields[0].ty.array_element_type().unwrap();
    let Type::Enum(e) = bucket else {
        return Err(invalid("Invalid Map bucket type"));
    };
    let empty = bucket.enum_variant("Empty").unwrap().0 as u64;
    let removed = bucket.enum_variant("Deleted").unwrap().0 as u64;
    let full = bucket.enum_variant("Full").unwrap().0;
    let width = bucket.scalar_types().len();
    let bytes = count
        .checked_mul(std::mem::size_of::<TaskValue>())
        .and_then(|n| n.checked_add(width * 8))
        .ok_or_else(|| invalid("Map snapshot size overflow"))?;
    let _scratch = reserve_current(bytes)?;
    let buckets = &*(words[fields[0].offset] as *const Buffer);
    let hashes = &*(words[fields[3].offset] as *const Buffer);
    let positions = &*(words[fields[4].offset] as *const Buffer);
    let mut raw = vec![0; width];
    let mut start = None;
    for i in 0..count {
        raw.fill(0);
        read_memory(bucket, buckets.load(i as u64)?, &mut raw)?;
        if raw[0] == empty {
            start = Some(i);
            break;
        }
    }
    let start = start.ok_or_else(|| invalid("Map has no empty bucket"))?;
    let mask = count - 1;
    let mut cluster = start;
    let mut live = 0;
    let mut tombstones = 0u64;
    // Starting at an empty bucket makes cyclic probe runs linear. A cached hash
    // must start within the current nonempty run, at or before its resident bucket.
    for step in 0..count {
        let position = (start + step) & mask;
        raw.fill(0);
        read_memory(bucket, buckets.load(position as u64)?, &mut raw)?;
        if raw[0] == empty {
            cluster = (position + 1) & mask;
        } else if raw[0] == removed {
            tombstones += 1;
        } else if raw[0] == full as u64 {
            let index = raw[e.variants[full].offset];
            let mut reverse = [0];
            let mut hash = [0];
            read_memory(&Type::UInt64, positions.load(index)?, &mut reverse)?;
            read_memory(&Type::UInt64, hashes.load(index)?, &mut hash)?;
            if reverse[0] != position as u64
                || ((hash[0] as usize).wrapping_sub(cluster) & mask)
                    > (position.wrapping_sub(cluster) & mask)
            {
                return Err(invalid("Invalid Map reverse index or probe path"));
            }
            live += 1;
        } else {
            return Err(invalid("Invalid Map bucket tag"));
        }
    }
    if live != length || tombstones != deleted {
        return Err(invalid("Invalid Map live or deleted count"));
    }
    Ok(())
}
