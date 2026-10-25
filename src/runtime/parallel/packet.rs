//! Backend-independent flat packets. Only active String/Array leaves own storage.
use super::{Lease, OwnedBuffer};
use crate::{
    runtime::{
        arena::Arena,
        buffer::Buffer,
        cleanup::{Cleanups, Entry},
    },
    Error, ErrorCode, ErrorPhase, Type,
};
use std::ptr::NonNull;

pub(crate) fn invalid() -> Error {
    Error::new_with_code(
        ErrorPhase::Abi,
        ErrorCode::InvalidCall,
        "Invalid parallel value packet",
    )
}

#[derive(Clone, Debug)]
pub(crate) enum Shape {
    Unit,
    Scalar(Type),
    String,
    Buffer,
    Product(Vec<Shape>),
    Sum(Vec<Shape>),
}
impl Shape {
    pub(crate) fn of(ty: &Type) -> Result<Self, Error> {
        fn build(ty: &Type, depth: usize) -> Result<Shape, Error> {
            if depth > 32 || ty.is_place() {
                return Err(invalid());
            }
            Ok(match ty {
                Type::Unit => Shape::Unit,
                Type::String => Shape::String,
                Type::Buffer(_) => Shape::Buffer,
                Type::Tuple(t) => Shape::Product(
                    t.elements
                        .iter()
                        .map(|t| build(t, depth + 1))
                        .collect::<Result<_, _>>()?,
                ),
                Type::Record(r) if !r.indirect && r.closure.is_none() => Shape::Product(
                    r.fields
                        .iter()
                        .map(|f| build(&f.ty, depth + 1))
                        .collect::<Result<_, _>>()?,
                ),
                Type::Enum(e) if !e.indirect && !e.variants.is_empty() => Shape::Sum(
                    e.variants
                        .iter()
                        .map(|v| {
                            v.payload
                                .as_ref()
                                .map_or(Ok(Shape::Unit), |t| build(t, depth + 1))
                        })
                        .collect::<Result<_, _>>()?,
                ),
                _ if crate::core::instance::scalar(ty) => Shape::Scalar(ty.clone()),
                _ => return Err(invalid()),
            })
        }
        build(ty, 0)
    }
    pub(crate) fn aggregate(&self) -> bool {
        matches!(self, Self::Product(_) | Self::Sum(_))
    }
    pub(crate) fn lanes(&self) -> usize {
        match self {
            Self::Unit => 0,
            Self::Scalar(_) | Self::String | Self::Buffer => 1,
            Self::Product(v) => v.iter().map(Self::lanes).sum(),
            Self::Sum(v) => 1 + v.iter().map(Self::lanes).sum::<usize>(),
        }
    }
    pub(crate) fn inputs(&self) -> usize {
        if matches!(self, Self::Unit) {
            1
        } else {
            self.lanes()
        }
    }
    pub(crate) fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + match self {
                Self::Product(v) | Self::Sum(v) => v.iter().map(Self::bytes).sum(),
                _ => 0,
            }
    }
    pub(crate) fn owners(&self) -> Vec<usize> {
        fn walk(s: &Shape, base: usize, out: &mut Vec<usize>) {
            match s {
                Shape::String | Shape::Buffer => out.push(base),
                Shape::Product(v) | Shape::Sum(v) => {
                    let mut offset = base + usize::from(matches!(s, Shape::Sum(_)));
                    for s in v {
                        walk(s, offset, out);
                        offset += s.lanes();
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        walk(self, 0, &mut out);
        out
    }
    /// Inactive enum lanes are never interpreted as pointers or scalar values.
    pub(crate) fn normalize(&self, words: &mut [u64]) -> Result<(), Error> {
        if words.len() != self.lanes() {
            return Err(invalid());
        }
        match self {
            Self::Unit => {}
            Self::String | Self::Buffer if words[0] != 0 => {}
            Self::String | Self::Buffer => return Err(invalid()),
            Self::Scalar(ty) => {
                words[0] = crate::core::instance::InstanceValue::from_bits(ty, words[0])
                    .ok_or_else(invalid)?
                    .bits();
            }
            Self::Product(v) | Self::Sum(v) => {
                let tag = if matches!(self, Self::Sum(_)) {
                    let tag = usize::try_from(words[0]).map_err(|_| invalid())?;
                    if tag >= v.len() {
                        return Err(invalid());
                    }
                    Some(tag)
                } else {
                    None
                };
                let mut offset = usize::from(tag.is_some());
                for (index, shape) in v.iter().enumerate() {
                    let end = offset + shape.lanes();
                    if tag.is_none_or(|tag| tag == index) {
                        shape.normalize(&mut words[offset..end])?;
                    } else {
                        words[offset..end].fill(0);
                    }
                    offset = end;
                }
            }
        }
        Ok(())
    }
    pub(crate) fn encode(&self, out: &mut Vec<u64>) {
        match self {
            Self::Unit => out.push(0),
            Self::String => out.push(14),
            Self::Buffer => out.push(18),
            Self::Scalar(ty) => out.push(match ty {
                Type::Bool => 1,
                Type::Int8 => 2,
                Type::UInt8 => 3,
                Type::Int16 => 4,
                Type::UInt16 => 5,
                Type::Int32 => 6,
                Type::UInt32 => 7,
                Type::Rune => 8,
                Type::Float32 => 9,
                Type::Int64 => 10,
                Type::UInt64 => 11,
                Type::Float64 => 12,
                _ => unreachable!("checked scalar shape"),
            }),
            Self::Product(v) | Self::Sum(v) => {
                out.extend([
                    if matches!(self, Self::Sum(_)) { 17 } else { 16 },
                    v.len() as u64,
                ]);
                for shape in v {
                    shape.encode(out);
                }
            }
        }
    }
    pub(crate) fn decode(words: &[u64]) -> Result<Self, Error> {
        fn read(words: &mut &[u64], depth: usize) -> Result<Shape, Error> {
            if depth > 32 {
                return Err(invalid());
            }
            let (&tag, rest) = words.split_first().ok_or_else(invalid)?;
            *words = rest;
            let scalar = match tag {
                0 => return Ok(Shape::Unit),
                14 => return Ok(Shape::String),
                18 => return Ok(Shape::Buffer),
                1 => Type::Bool,
                2 => Type::Int8,
                3 => Type::UInt8,
                4 => Type::Int16,
                5 => Type::UInt16,
                6 => Type::Int32,
                7 => Type::UInt32,
                8 => Type::Rune,
                9 => Type::Float32,
                10 => Type::Int64,
                11 => Type::UInt64,
                12 => Type::Float64,
                16 | 17 => {
                    let (&count, rest) = words.split_first().ok_or_else(invalid)?;
                    *words = rest;
                    let count = usize::try_from(count).map_err(|_| invalid())?;
                    if count > words.len() || (tag == 17 && count == 0) {
                        return Err(invalid());
                    }
                    let children = (0..count)
                        .map(|_| read(words, depth + 1))
                        .collect::<Result<_, _>>()?;
                    return Ok(if tag == 16 {
                        Shape::Product(children)
                    } else {
                        Shape::Sum(children)
                    });
                }
                _ => return Err(invalid()),
            };
            Ok(Shape::Scalar(scalar))
        }
        let mut rest = words;
        let shape = read(&mut rest, 0)?;
        if !rest.is_empty() {
            return Err(invalid());
        }
        Ok(shape)
    }
}

pub(crate) struct Packet {
    pub(crate) shape: Shape,
    pub(crate) words: Vec<u64>,
    owners: Vec<(usize, OwnedBuffer)>,
    _charge: Option<Lease>,
}
impl Packet {
    pub(crate) fn empty(shape: Shape) -> Result<Self, Error> {
        let lanes = shape.lanes();
        let charge = super::reserve_current(
            std::mem::size_of::<Self>()
                + shape.bytes()
                // Cover lanes, capture/install scratch and the owner index list.
                + lanes * (32 + std::mem::size_of::<(usize, OwnedBuffer)>()),
        )?;
        Ok(Self {
            shape,
            words: vec![0; lanes],
            owners: Vec::with_capacity(lanes),
            _charge: charge,
        })
    }
    /// Consume a complete compiler-owned value, releasing remaining owners on failure.
    /// # Safety
    /// Every active String/Array lane is live and exclusively owned by `entry`.
    pub(crate) unsafe fn capture(
        shape: Shape,
        mut words: Vec<u64>,
        mut entry: Option<Entry>,
        arena: &mut Arena,
    ) -> Result<Self, Error> {
        if let Some(entry) = &mut entry {
            entry.materialize();
        }
        let result = (|| {
            shape.normalize(&mut words)?;
            let mut packet = Self::empty(shape)?;
            for index in packet.shape.owners() {
                if words[index] == 0 {
                    continue;
                }
                let raw = words[index] as *mut Buffer;
                let source = entry
                    .as_mut()
                    .and_then(|e| e.owners.iter_mut().find(|o| **o == Some(raw)))
                    .ok_or_else(invalid)?;
                let owner = OwnedBuffer::new(arena, NonNull::new(raw).ok_or_else(invalid)?)?;
                *source = None;
                packet.owners.push((index, owner));
            }
            packet.words = words;
            Ok(packet)
        })();
        let cleanup = entry.map_or(Ok(()), |e| e.run(arena));
        match (result, cleanup) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(e), _) | (_, Err(e)) => Err(e),
        }
    }
    /// Native returns have already detached their cleanup registration.
    /// # Safety
    /// `words` is the successful generated return, whose owners are now exclusive.
    pub(crate) unsafe fn returned(
        shape: Shape,
        mut words: Vec<u64>,
        arena: &mut Arena,
    ) -> Result<Self, Error> {
        shape.normalize(&mut words)?;
        let entry = Entry {
            keep_alive: None,
            storage_size: 0,
            storage: None,
            owners: shape
                .owners()
                .iter()
                .map(|&i| NonNull::new(words[i] as *mut Buffer).map(NonNull::as_ptr))
                .collect(),
            hooks: vec![],
        };
        Self::capture(shape, words, Some(entry), arena)
    }
    pub(crate) fn install(
        mut self,
        arena: &Arena,
        cleanups: &mut Cleanups,
        base: usize,
        key: Option<usize>,
    ) -> Result<Vec<u64>, Error> {
        let indices = self.shape.owners();
        if let Some(&first) = indices.first() {
            // Publish the cleanup group before any fallible owner handoff.
            let key = key.unwrap_or(base + first);
            let mut entry = Entry {
                keep_alive: None,
                storage_size: 0,
                storage: None,
                owners: vec![None; indices.len()],
                hooks: vec![],
            };
            for (index, owner) in &mut self.owners {
                match owner.take(arena) {
                    Ok(raw) => {
                        self.words[*index] = raw as u64;
                        entry.owners[indices.binary_search(index).unwrap()] = Some(raw);
                    }
                    Err(error) => {
                        cleanups.insert(key, entry);
                        return Err(error);
                    }
                }
            }
            cleanups.insert(key, entry);
        }
        Ok(self.words)
    }
    pub(crate) fn push_text(
        &mut self,
        index: usize,
        text: &str,
        arena: &mut Arena,
    ) -> Result<(), Error> {
        let raw = Buffer::copy_bytes(arena, text.as_bytes())?;
        unsafe { self.push_buffer(index, raw, arena) }
    }
    /// Adopt a freshly initialized exclusive buffer, releasing it on failure.
    /// # Safety
    /// `raw` is a live exclusive buffer in `arena`; this call consumes its cleanup
    /// responsibility on both success and failure. No other owner may release it.
    pub(crate) unsafe fn push_buffer(
        &mut self,
        index: usize,
        raw: NonNull<Buffer>,
        arena: &mut Arena,
    ) -> Result<(), Error> {
        let owner = unsafe { OwnedBuffer::new(arena, raw) }.inspect_err(|_| {
            unsafe { Buffer::release(arena, raw.as_ptr()) }.expect("new buffer cleanup");
        })?;
        self.words[index] = raw.as_ptr() as u64;
        self.owners.push((index, owner));
        Ok(())
    }
}
