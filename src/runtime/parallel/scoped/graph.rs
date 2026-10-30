//! Compiler-authored storage fields. Owning lanes are replaced with local views;
//! neither foreign headers nor uninitialized aggregate padding enter a packet.
use super::*;

const LIMIT: usize = 65_536;

#[derive(Clone, Debug)]
pub(super) struct Element {
    pub layout: Layout,
    fields: Vec<Field>,
    owners: Vec<usize>,
}
#[derive(Clone, Debug)]
struct Field {
    offset: usize,
    kind: Kind,
}
#[derive(Clone, Debug)]
enum Kind {
    Bytes(usize),
    SharedVm,
    Owner(Arc<Element>),
    Enum(Layout, Vec<(usize, Element)>),
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
                    Type::Resource(name) if name.as_ref() == "vm.Owner" => out.push(Field {
                        offset,
                        kind: Kind::SharedVm,
                    }),
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
                    Type::Enum(e) => {
                        let crate::memory_layout::MemoryLayoutShape::Enum { variants, .. } =
                            layout.shape
                        else {
                            return None;
                        };
                        let mut payloads = Vec::with_capacity(variants.len());
                        let mut end = 4;
                        for (variant, location) in e.variants.iter().zip(variants) {
                            let payload = build(
                                variant.payload.as_ref().unwrap_or(&Type::Unit),
                                depth + 1,
                                work,
                            )?;
                            let offset = location.payload.map_or(end, |p| p.offset);
                            end = offset + payload.layout.size();
                            payloads.push((offset, payload));
                        }
                        out.push(Field {
                            offset,
                            kind: Kind::Enum(
                                Shape::checked(layout.size, layout.align).ok()?.element,
                                payloads,
                            ),
                        });
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
            Some(Element::new(layout, fields))
        }
        build(ty, 0, &mut 0)
    }
    fn new(layout: Layout, fields: Vec<Field>) -> Self {
        let mut owners = Vec::new();
        for field in &fields {
            match &field.kind {
                Kind::Owner(_) | Kind::SharedVm => owners.push(field.offset),
                Kind::Enum(_, variants) => {
                    for (offset, payload) in variants {
                        owners.extend(payload.owners().map(|owner| field.offset + offset + owner));
                    }
                }
                Kind::Bytes(_) => {}
            }
        }
        Self {
            layout,
            fields,
            owners,
        }
    }
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + 2 * std::mem::size_of::<usize>()
            + self.fields.capacity() * std::mem::size_of::<Field>()
            + self.owners.capacity() * std::mem::size_of::<usize>()
            + self
                .fields
                .iter()
                .map(|field| match &field.kind {
                    Kind::Owner(element) => element.bytes(),
                    Kind::Enum(_, variants) => {
                        variants.capacity() * std::mem::size_of::<(usize, Element)>()
                            + variants.iter().map(|(_, e)| e.bytes()).sum::<usize>()
                    }
                    Kind::Bytes(_) | Kind::SharedVm => 0,
                })
                .sum::<usize>()
    }
    pub fn structured(&self) -> bool {
        self.fields
            .iter()
            .any(|f| !matches!(f.kind, Kind::Bytes(_)))
    }
    pub fn owners(&self) -> impl Iterator<Item = usize> + '_ {
        self.owners.iter().copied()
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
                Kind::SharedVm => words.push(11),
                Kind::Owner(element) => {
                    words.push(9);
                    element.encode(words);
                }
                Kind::Enum(layout, variants) => {
                    words.extend([
                        10,
                        layout.size() as u64,
                        layout.align() as u64,
                        variants.len() as u64,
                    ]);
                    for (offset, payload) in variants {
                        words.push(*offset as u64);
                        payload.encode(words);
                    }
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
            let (kind, width, alignment) = match tag {
                1 | 2 | 4 | 8 => (Kind::Bytes(tag), tag, tag),
                11 => (Kind::SharedVm, 8, 8),
                9 => (
                    Kind::Owner(Arc::new(Self::decode(words, depth + 1, work)?)),
                    std::mem::size_of::<usize>(),
                    std::mem::align_of::<usize>(),
                ),
                10 => {
                    let size = word(words)?;
                    let align = word(words)?;
                    let layout = Shape::checked(size, align)?.element;
                    let count = word(words)?;
                    if size < 4
                        || align < 4
                        || count == 0
                        || count > LIMIT
                        || count > words.len() / 4
                    {
                        return Err(invalid());
                    }
                    let mut variants = Vec::with_capacity(count);
                    let mut end = 4;
                    for _ in 0..count {
                        let offset = word(words)?;
                        let payload = Self::decode(words, depth + 1, work)?;
                        if offset < end
                            || !offset.is_multiple_of(payload.layout.align())
                            || payload.layout.align() > align
                        {
                            return Err(invalid());
                        }
                        end = offset
                            .checked_add(payload.layout.size())
                            .filter(|end| *end <= size)
                            .ok_or_else(invalid)?;
                        variants.push((offset, payload));
                    }
                    (Kind::Enum(layout, variants), size, align)
                }
                _ => return Err(invalid()),
            };
            if offset < end || !offset.is_multiple_of(alignment) || alignment > align {
                return Err(invalid());
            }
            end = offset
                .checked_add(width)
                .filter(|end| *end <= size)
                .ok_or_else(invalid)?;
            fields.push(Field { offset, kind });
        }
        Ok(Self::new(layout, fields))
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
            let base = data.as_ptr().add(i * self.layout.size());
            for f in &self.fields {
                let source = base.add(f.offset);
                match &f.kind {
                    Kind::Bytes(_) => {}
                    Kind::SharedVm => {
                        if !Shape::vm().eligible(source.cast::<u64>().read()) {
                            return false;
                        }
                    }
                    Kind::Owner(element) => {
                        let owner = source.cast::<*const Buffer>().read();
                        let Some(buffer) = owner.as_ref() else {
                            return false;
                        };
                        let Some(source) = buffer.scoped_elements(element.layout, element.owners())
                        else {
                            return false;
                        };
                        if element.structured() && !element.eligible(source.data, source.len, work)
                        {
                            return false;
                        }
                    }
                    Kind::Enum(_, variants) => {
                        let tag = source.cast::<i32>().read();
                        let Some((offset, payload)) =
                            usize::try_from(tag).ok().and_then(|tag| variants.get(tag))
                        else {
                            return false;
                        };
                        if !payload.eligible(NonNull::new_unchecked(source.add(*offset)), 1, work) {
                            return false;
                        }
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
            let offset = i * self.layout.size();
            self.capture_fields(data.as_ptr().add(offset), offset, scope, &mut snapshot)?;
        }
        Ok(snapshot)
    }
    unsafe fn capture_fields(
        &self,
        data: *const u8,
        base: usize,
        scope: &Arc<Scope>,
        snapshot: &mut Snapshot,
    ) -> Result<(), Error> {
        for f in &self.fields {
            let offset = base + f.offset;
            let source = data.add(f.offset);
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
                        shared_vm: false,
                        graph: Some(element.clone()),
                    };
                    let range = Range::capture(
                        &shape,
                        source.cast::<usize>().read() as u64,
                        scope.clone(),
                    )?;
                    snapshot.children.push((offset, range));
                }
                Kind::SharedVm => {
                    snapshot.children.push((
                        offset,
                        Range::capture(&Shape::vm(), source.cast::<u64>().read(), scope.clone())?,
                    ));
                }
                Kind::Enum(_, variants) => {
                    let tag = source.cast::<i32>().read();
                    let (child, payload) = usize::try_from(tag)
                        .ok()
                        .and_then(|tag| variants.get(tag))
                        .ok_or_else(invalid)?;
                    snapshot
                        .words
                        .as_mut_ptr()
                        .cast::<u8>()
                        .add(offset)
                        .cast::<i32>()
                        .write(tag);
                    payload.capture_fields(source.add(*child), offset + child, scope, snapshot)?;
                }
            }
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn enum_graph() -> Element {
        let bytes = Element::new(
            Layout::from_size_align(1, 1).unwrap(),
            vec![Field {
                offset: 0,
                kind: Kind::Bytes(1),
            }],
        );
        let text = Element::new(
            Layout::from_size_align(8, 8).unwrap(),
            vec![Field {
                offset: 0,
                kind: Kind::Owner(Arc::new(bytes)),
            }],
        );
        let empty = Element::new(Layout::from_size_align(0, 1).unwrap(), vec![]);
        let layout = Layout::from_size_align(16, 8).unwrap();
        Element::new(
            layout,
            vec![Field {
                offset: 0,
                kind: Kind::Enum(layout, vec![(4, empty), (8, text)]),
            }],
        )
    }

    #[test]
    fn scoped_borrow_enum_graph_skips_inactive_owners_and_checks_tags() {
        let graph = enum_graph();
        assert_eq!(graph.owners().collect::<Vec<_>>(), [8]);
        // The inactive payload is deliberately not a dereferenceable pointer.
        let mut data = [0u64, u64::MAX];
        let pointer = NonNull::new(data.as_mut_ptr().cast::<u8>()).unwrap();
        unsafe {
            assert!(graph.eligible(pointer, 1, &mut 0));
            pointer.as_ptr().cast::<i32>().write(-1);
            assert!(!graph.eligible(pointer, 1, &mut 0));
            pointer.as_ptr().cast::<i32>().write(2);
            assert!(!graph.eligible(pointer, 1, &mut 0));
            pointer.as_ptr().cast::<i32>().write(1);
            pointer.as_ptr().add(8).cast::<usize>().write(0);
            assert!(!graph.eligible(pointer, 1, &mut 0));
        }
    }

    #[test]
    fn scoped_borrow_enum_descriptors_reject_overlapping_variants_and_bad_layouts() {
        let graph = enum_graph();
        let mut encoded = Vec::new();
        graph.encode(&mut encoded);
        let mut source = encoded.as_slice();
        let decoded = Element::decode(&mut source, 0, &mut 0).unwrap();
        assert!(source.is_empty());
        assert_eq!(decoded.owners().collect::<Vec<_>>(), [8]);
        for length in 0..encoded.len() {
            assert!(Element::decode(&mut &encoded[..length], 0, &mut 0).is_err());
        }
        // Enum extent, alignment, variant count and payload regions.
        for (index, value) in [
            (5, 0),
            (6, 16),
            (7, 0),
            (7, u64::MAX),
            (8, 0),
            (12, 4),
            (12, 16),
        ] {
            let mut bad = encoded.clone();
            bad[index] = value;
            assert!(
                Element::decode(&mut bad.as_slice(), 0, &mut 0).is_err(),
                "{index}={value}"
            );
        }
    }
}
