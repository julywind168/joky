//! Backend-independent lanes, exclusive String/Array owners and inline snapshots.
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
    Place(super::place::Layout),
    Scalar(Type),
    String,
    // A compiler-generated value under the frozen module's closed target proof.
    // This shape is never accepted at the public TaskValue or host ABI boundary.
    Function,
    // Compiler-created move closures retain a null owner identity.
    EmptyOwner,
    Buffer,
    Product(Vec<Shape>),
    Sum(Vec<Shape>),
}
impl Shape {
    pub(crate) fn of(ty: &Type) -> Result<Self, Error> {
        fn build(ty: &Type, depth: usize) -> Result<Shape, Error> {
            if depth > 32 {
                return Err(invalid());
            }
            if ty.is_place() {
                return Ok(Shape::Place(super::place::Layout::of(ty)?));
            }
            Ok(match ty {
                Type::Unit => Shape::Unit,
                Type::String => Shape::String,
                Type::Function(_) => Shape::Function,
                Type::Named(n) if matches!(n.as_str(), "domain.Handle" | "admission.Token") => {
                    Shape::Scalar(Type::UInt64)
                }
                Type::Buffer(_) | Type::DynTrait(_) => Shape::Buffer,
                Type::Resource(name)
                    if (crate::runtime::tcp::transferable_type(name)
                        || crate::runtime::parallel::control::resource_type(name)) =>
                {
                    Shape::Buffer
                }
                Type::Tuple(t) => Shape::Product(
                    t.elements
                        .iter()
                        .map(|t| build(t, depth + 1))
                        .collect::<Result<_, _>>()?,
                ),
                Type::Record(r) if r.closure.as_ref().is_some_and(|c| c.storable) => {
                    let mut fields = vec![Shape::EmptyOwner];
                    fields.extend(
                        r.fields
                            .iter()
                            .skip(1)
                            .map(|f| build(&f.ty, depth + 1))
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                    Shape::Product(fields)
                }
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
        matches!(self, Self::Product(_) | Self::Sum(_) | Self::Place(_))
    }
    pub(crate) fn lanes(&self) -> usize {
        match self {
            Self::Unit => 0,
            Self::Scalar(_)
            | Self::String
            | Self::Function
            | Self::EmptyOwner
            | Self::Buffer
            | Self::Place(_) => 1,
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
                Self::Place(layout) => layout.bytes(),
                _ => 0,
            }
    }
    pub(crate) fn owners(&self) -> Vec<usize> {
        fn walk(s: &Shape, base: usize, out: &mut Vec<usize>) {
            match s {
                Shape::String | Shape::Buffer | Shape::EmptyOwner => out.push(base),
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
            Self::Unit | Self::Function => {}
            Self::EmptyOwner if words[0] == 0 => {}
            Self::EmptyOwner => return Err(invalid()),
            Self::String | Self::Buffer | Self::Place(_) if words[0] != 0 => {}
            Self::String | Self::Buffer | Self::Place(_) => return Err(invalid()),
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
            Self::Place(layout) => {
                out.extend([
                    19,
                    layout.size as u64,
                    layout.align as u64,
                    layout.nodes.len() as u64,
                ]);
                for n in &layout.nodes {
                    out.extend([n.offset, n.kind, n.count, n.stride, n.end]);
                }
            }
            Self::Unit => out.push(0),
            Self::Function => out.push(20),
            Self::EmptyOwner => out.push(21),
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
                20 => return Ok(Shape::Function),
                21 => return Ok(Shape::EmptyOwner),
                19 => {
                    if words.len() < 3 {
                        return Err(invalid());
                    }
                    let (size, align, count) =
                        (words[0] as usize, words[1] as usize, words[2] as usize);
                    *words = &words[3..];
                    if count > words.len() / 5 {
                        return Err(invalid());
                    }
                    let nodes = words[..count * 5]
                        .as_chunks::<5>()
                        .0
                        .iter()
                        .map(|n| crate::runtime::place::CopyNode {
                            offset: n[0],
                            kind: n[1],
                            count: n[2],
                            stride: n[3],
                            end: n[4],
                        })
                        .collect();
                    *words = &words[count * 5..];
                    let layout = super::place::Layout { size, align, nodes };
                    layout.validate()?;
                    return Ok(Shape::Place(layout));
                }
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
                    let children: Vec<_> = (0..count)
                        .map(|_| read(words, depth + 1))
                        .collect::<Result<_, _>>()?;
                    if children.iter().any(|s| matches!(s, Shape::Place(_))) {
                        return Err(invalid());
                    }
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
    place: Option<Vec<u64>>,
    cleanup: Option<super::portable::Payload>,
    _charge: Option<Lease>,
}
impl Packet {
    pub(crate) fn empty(shape: Shape) -> Result<Self, Error> {
        let lanes = shape.lanes();
        let owners = match &shape {
            Shape::Place(layout) => layout.owner_count(),
            _ => lanes,
        };
        let charge = super::reserve_current(
            std::mem::size_of::<Self>()
                + shape.bytes()
                + match &shape { Shape::Place(layout) => layout.size.max(1).div_ceil(8) * 8, _ => 0 }
                // Cover lanes, capture/install scratch and the owner index list.
                + lanes * 32 + owners * std::mem::size_of::<(usize, OwnedBuffer)>(),
        )?;
        Ok(Self {
            shape,
            words: vec![0; lanes],
            owners: Vec::with_capacity(owners),
            place: None,
            cleanup: None,
            _charge: charge,
        })
    }
    /// Consume a complete compiler-owned value, releasing remaining owners on failure.
    /// # Safety
    /// Every active String/Array lane is live and exclusively owned by `entry`.
    /// A place lane points to a fully initialized value of the checked layout.
    /// Its storage stays with the caller, which releases any owners not adopted
    /// here. Adopted owning lanes are cleared in the exclusively writable source.
    pub(crate) unsafe fn capture(
        shape: Shape,
        words: Vec<u64>,
        entry: Option<Entry>,
        arena: &mut Arena,
    ) -> Result<Self, Error> {
        Self::capture_value(shape, words, entry, arena, false)
    }
    pub(crate) unsafe fn capture_value(
        shape: Shape,
        mut words: Vec<u64>,
        mut entry: Option<Entry>,
        arena: &mut Arena,
        returned: bool,
    ) -> Result<Self, Error> {
        let portable = entry
            .as_ref()
            .map_or(Ok(false), |e| super::portable::Payload::needed(e, arena));
        match portable {
            Ok(false) => {}
            result => {
                let captured = (|| {
                    result?;
                    shape.normalize(&mut words)?;
                    let mut packet = Self::empty(shape)?;
                    packet.cleanup = Some(super::portable::Payload::capture(
                        entry.as_ref().unwrap(),
                        arena,
                        returned,
                    )?);
                    packet.words = words;
                    Ok(packet)
                })();
                if captured.is_err() {
                    if let Some(entry) = entry {
                        let _ = entry.run(arena);
                    }
                }
                return captured;
            }
        }
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
            if let Shape::Place(layout) = &packet.shape {
                let source = words[0] as *mut u8;
                let mut snapshot = layout.snapshot(source);
                let mut error = None;
                crate::runtime::place::owners(
                    snapshot.as_mut_ptr().cast(),
                    &layout.nodes,
                    &mut |lane| {
                        let raw = lane.read_unaligned() as *mut Buffer;
                        let offset = lane as usize - snapshot.as_ptr() as usize;
                        if raw.is_null() || error.is_some() {
                            return;
                        }
                        match OwnedBuffer::new(arena, NonNull::new_unchecked(raw)) {
                            Ok(owner) => {
                                source.add(offset).cast::<usize>().write_unaligned(0);
                                packet.owners.push((offset, owner));
                            }
                            Err(next) => {
                                error = Some(next);
                            }
                        }
                    },
                );
                if let Some(error) = error {
                    return Err(error);
                }
                words[0] = snapshot.as_ptr() as u64;
                packet.place = Some(snapshot);
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
    /// A returned place stays live in its completed frame through this call.
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
        arena: &mut Arena,
        cleanups: &mut Cleanups,
        base: usize,
        key: Option<usize>,
    ) -> Result<Vec<u64>, Error> {
        if let Some(cleanup) = self.cleanup.take() {
            let key = key.unwrap_or(base + self.shape.owners()[0]);
            cleanups.insert(key, cleanup.install(arena)?);
            return Ok(self.words);
        }
        if let Shape::Place(layout) = &self.shape {
            let destination =
                cleanups
                    .places
                    .materialize(arena, base, layout.size, layout.align)?;
            cleanups.places.describe(arena, base, &layout.nodes)?;
            unsafe {
                self.install_place(arena, destination.as_ptr())?;
            }
            self.words[0] = destination.as_ptr() as u64;
        }
        let indices = self.shape.owners();
        if let Some(&first) = indices.first() {
            // Publish the cleanup group before any fallible owner handoff.
            let key = key.unwrap_or(base + first);
            let mut entry = Entry {
                keep_alive: None,
                storage_size: 0,
                storage: None,
                owners: (0..indices.len()).map(|_| None).collect(),
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
    /// # Safety
    /// The destination has this layout and a live cleanup registration in `arena`,
    /// and is exclusively writable throughout the fallible handoff.
    pub(crate) unsafe fn install_place(
        &mut self,
        arena: &mut Arena,
        destination: *mut u8,
    ) -> Result<(), Error> {
        let Shape::Place(layout) = &self.shape else {
            return Err(invalid());
        };
        crate::runtime::place::dispose(arena, destination, &layout.nodes)?;
        crate::runtime::place::copy(destination, self.words[0] as *const u8, &layout.nodes);
        crate::runtime::place::owners(destination, &layout.nodes, &mut |lane| {
            lane.write_unaligned(0)
        });
        for (offset, owner) in &mut self.owners {
            let raw = owner.take(arena)?;
            destination
                .add(*offset)
                .cast::<*mut Buffer>()
                .write_unaligned(raw);
        }
        Ok(())
    }
    /// Initialize the already charged inline storage for a checked host value.
    pub(crate) fn place_storage(&mut self) -> *mut u8 {
        let Shape::Place(layout) = &self.shape else {
            unreachable!("Place packet");
        };
        let data = self
            .place
            .get_or_insert_with(|| vec![0u64; layout.size.max(1).div_ceil(8)]);
        self.words[0] = data.as_ptr() as u64;
        data.as_mut_ptr().cast()
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
