//! Compiler-authored storage fields. Owning lanes are replaced with local views;
//! neither foreign headers nor uninitialized aggregate padding enter a packet.
use super::*;

const LIMIT: usize = 65_536;

#[derive(Clone, Debug)]
pub(super) struct Element {
    pub layout: Layout,
    fields: Vec<Field>,
}
#[derive(Clone, Debug)]
struct Field {
    offset: usize,
    kind: Kind,
}
#[derive(Clone, Debug)]
enum Kind {
    Bytes(usize),
    Owner(Arc<Element>),
}

impl Element {
    pub fn of(ty: &Type) -> Option<Self> {
        fn build(ty: &Type, depth: usize, work: &mut usize) -> Option<Element> {
            if depth > 32 {
                return None;
            }
            let layout = ty.storage_layout().ok()?;
            let layout = Shape::checked(layout.size, layout.align).ok()?.element;
            let mut fields = Vec::new();
            fn flatten(
                ty: &Type,
                offset: usize,
                depth: usize,
                work: &mut usize,
                out: &mut Vec<Field>,
            ) -> Option<()> {
                *work += 1;
                if *work > LIMIT || depth > 32 {
                    return None;
                }
                let layout = ty.storage_layout().ok()?;
                match ty {
                    Type::Record(r) => {
                        let crate::memory_layout::MemoryLayoutShape::Record(locations) =
                            layout.shape
                        else {
                            return None;
                        };
                        for (field, location) in r.fields.iter().zip(locations) {
                            flatten(&field.ty, offset + location.offset, depth + 1, work, out)?;
                        }
                    }
                    Type::Tuple(t) => {
                        let crate::memory_layout::MemoryLayoutShape::Tuple(locations) =
                            layout.shape
                        else {
                            return None;
                        };
                        for (ty, location) in t.elements.iter().zip(locations) {
                            flatten(ty, offset + location.offset, depth + 1, work, out)?;
                        }
                    }
                    Type::FixedArray(a) => {
                        let crate::memory_layout::MemoryLayoutShape::Array {
                            stride, length, ..
                        } = layout.shape
                        else {
                            return None;
                        };
                        if stride != 0 {
                            for i in 0..length as usize {
                                flatten(&a.element, offset + i * stride, depth + 1, work, out)?;
                            }
                        }
                    }
                    Type::String | Type::Buffer(_) => {
                        let element = match ty {
                            Type::Buffer(e) => &**e,
                            _ => &Type::UInt8,
                        };
                        out.push(Field {
                            offset,
                            kind: Kind::Owner(Arc::new(build(element, depth + 1, work)?)),
                        });
                    }
                    _ if layout.size == 0 => {}
                    _ => out.push(Field {
                        offset,
                        kind: Kind::Bytes(layout.size),
                    }),
                }
                Some(())
            }
            flatten(ty, 0, depth, work, &mut fields)?;
            Some(Element { layout, fields })
        }
        build(ty, 0, &mut 0)
    }
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + 2 * std::mem::size_of::<usize>()
            + self.fields.capacity() * std::mem::size_of::<Field>()
            + self
                .fields
                .iter()
                .map(|field| match &field.kind {
                    Kind::Owner(element) => element.bytes(),
                    Kind::Bytes(_) => 0,
                })
                .sum::<usize>()
    }
    pub fn owning(&self) -> bool {
        self.fields.iter().any(|f| matches!(f.kind, Kind::Owner(_)))
    }
    pub fn owners(&self) -> impl Iterator<Item = usize> + '_ {
        self.fields
            .iter()
            .filter_map(|f| matches!(f.kind, Kind::Owner(_)).then_some(f.offset))
    }
    pub fn encode(&self, words: &mut Vec<u64>) {
        words.extend([
            self.layout.size() as u64,
            self.layout.align() as u64,
            self.fields.len() as u64,
        ]);
        for f in &self.fields {
            words.push(f.offset as u64);
            match &f.kind {
                Kind::Bytes(size) => words.push(*size as u64),
                Kind::Owner(element) => {
                    words.push(9);
                    element.encode(words);
                }
            }
        }
    }
    pub fn decode(words: &mut &[u64], depth: usize, work: &mut usize) -> Result<Self, Error> {
        fn word(words: &mut &[u64]) -> Result<usize, Error> {
            let (&head, tail) = words.split_first().ok_or_else(invalid)?;
            *words = tail;
            usize::try_from(head).map_err(|_| invalid())
        }
        if depth > 32 {
            return Err(invalid());
        }
        let size = word(words)?;
        let align = word(words)?;
        let layout = Shape::checked(size, align)?.element;
        let count = word(words)?;
        *work = count
            .checked_add(1)
            .and_then(|count| work.checked_add(count))
            .ok_or_else(invalid)?;
        if *work > LIMIT || count > words.len() / 2 {
            return Err(invalid());
        }
        let mut fields = Vec::new();
        let mut end = 0;
        for _ in 0..count {
            let offset = word(words)?;
            let tag = word(words)?;
            let (kind, width) = match tag {
                1 | 2 | 4 | 8 => (Kind::Bytes(tag), tag),
                9 => (
                    Kind::Owner(Arc::new(Self::decode(words, depth + 1, work)?)),
                    8,
                ),
                _ => return Err(invalid()),
            };
            if offset < end || !offset.is_multiple_of(width) || width > align {
                return Err(invalid());
            }
            end = offset
                .checked_add(width)
                .filter(|end| *end <= size)
                .ok_or_else(invalid)?;
            fields.push(Field { offset, kind });
        }
        Ok(Self { layout, fields })
    }
    /// # Safety
    /// Data must be live initialized storage of this compiler-certified element
    /// layout, owned or received by the current task, and frozen through capture.
    /// Only the sending worker traverses its own control blocks; each child
    /// runtime header must independently prove current-task provenance.
    pub unsafe fn eligible(&self, data: NonNull<u8>, len: usize, work: &mut usize) -> bool {
        let count = self.fields.len();
        let Some(next) = count.checked_mul(len).and_then(|n| work.checked_add(n)) else {
            return false;
        };
        if next > LIMIT {
            return false;
        }
        *work = next;
        for i in 0..len {
            for f in &self.fields {
                if let Kind::Owner(element) = &f.kind {
                    let owner = data
                        .as_ptr()
                        .add(i * self.layout.size() + f.offset)
                        .cast::<*const Buffer>()
                        .read();
                    let Some(buffer) = owner.as_ref() else {
                        return false;
                    };
                    let Some(source) = buffer.scoped_elements(element.layout, element.owners())
                    else {
                        return false;
                    };
                    if element.owning() && !element.eligible(source.data, source.len, work) {
                        return false;
                    }
                }
            }
        }
        true
    }
    /// # Safety
    /// Eligibility has checked this entire graph on the current worker. The
    /// source stays immutable and alive through all readers under `scope`.
    pub unsafe fn capture(
        &self,
        data: NonNull<u8>,
        len: usize,
        scope: &Arc<Scope>,
    ) -> Result<Snapshot, Error> {
        let bytes = len.checked_mul(self.layout.size()).ok_or_else(invalid)?;
        let count = len.checked_mul(self.owners().count()).ok_or_else(invalid)?;
        let words = bytes.div_ceil(8);
        let charge = words
            .checked_mul(8)
            .and_then(|n| n.checked_add(count.checked_mul(std::mem::size_of::<(usize, Range)>())?))
            .ok_or_else(invalid)?;
        let charge = super::super::reserve_current(charge + std::mem::size_of::<Snapshot>())?;
        let mut snapshot = Snapshot {
            words: vec![0u64; words],
            children: Vec::with_capacity(count),
            _charge: charge,
        };
        for i in 0..len {
            for f in &self.fields {
                let offset = i * self.layout.size() + f.offset;
                let source = data.as_ptr().add(offset);
                match &f.kind {
                    Kind::Bytes(size) => std::ptr::copy_nonoverlapping(
                        source,
                        snapshot.words.as_mut_ptr().cast::<u8>().add(offset),
                        *size,
                    ),
                    Kind::Owner(element) => {
                        let shape = Shape {
                            element: element.layout,
                            plain: false,
                            graph: Some(element.clone()),
                        };
                        let range =
                            Range::capture(&shape, source.cast::<u64>().read(), scope.clone())?;
                        snapshot.children.push((offset, range));
                    }
                }
            }
        }
        Ok(snapshot)
    }
}

pub(super) struct Snapshot {
    words: Vec<u64>,
    children: Vec<(usize, Range)>,
    _charge: Option<Lease>,
}
impl Snapshot {
    pub fn install(
        self,
        arena: &mut Arena,
        layout: Layout,
        len: usize,
        loans: &mut Vec<Rc<Loan>>,
    ) -> Result<NonNull<u8>, Error> {
        let bytes = len.checked_mul(layout.size()).ok_or_else(invalid)?;
        let data = arena.allocate(bytes, layout.align())?;
        unsafe {
            std::ptr::copy_nonoverlapping(self.words.as_ptr().cast::<u8>(), data.as_ptr(), bytes);
        }
        for (offset, range) in self.children {
            let word = range.install(arena, loans, false)?;
            unsafe {
                data.as_ptr().add(offset).cast::<u64>().write(word);
            }
        }
        Ok(data)
    }
}
