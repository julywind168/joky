//! Private, nonescaping inputs for a compiler-certified static join. Ordinary
//! packets and public TaskValue inputs never accept references.
use super::{
    packet::{invalid, Packet, Shape as OwnedShape},
    pool, Lease, Storage,
};
use crate::{
    runtime::{arena::Arena, buffer::Buffer, cleanup::Cleanups},
    Error, Type,
};
use std::{alloc::Layout, ptr::NonNull, sync::Arc};

#[derive(Clone, Debug)]
pub(crate) struct Shape {
    element: Type,
}
impl Shape {
    pub(crate) fn of(ty: &Type) -> Option<Self> {
        let reference = ty.reference_type()?;
        if reference.mutable {
            return None;
        }
        let element = match &reference.target {
            Type::String => Type::UInt8,
            Type::BufferSlice(element) => (**element).clone(),
            Type::Record(record)
                if record.array && !record.indirect && record.fields.len() == 1 =>
            {
                reference.target.array_element_type()?.clone()
            }
            _ => return None,
        };
        Self::tag(&element)?;
        Some(Self { element })
    }
    fn tag(ty: &Type) -> Option<u64> {
        Some(match ty {
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
            _ => return None,
        })
    }
    pub(crate) fn encode(&self, words: &mut Vec<u64>) {
        words.extend([99, Self::tag(&self.element).unwrap()]);
    }
    pub(crate) fn decode(words: &[u64]) -> Result<Self, Error> {
        let [99, tag] = *words else {
            return Err(invalid());
        };
        let element = match tag {
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
            _ => return Err(invalid()),
        };
        Ok(Self { element })
    }
    pub(crate) fn layout(&self) -> Layout {
        let layout = self.element.storage_layout().unwrap();
        Layout::from_size_align(layout.size, layout.align).unwrap()
    }
    /// Compiler layout proves a live shared reference. String, Slice and the
    /// single-field Array wrapper all name a Buffer header directly.
    pub(crate) unsafe fn buffer(&self, word: u64) -> *const Buffer {
        word as *const Buffer
    }
    pub(crate) unsafe fn eligible(&self, word: u64) -> bool {
        self.buffer(word)
            .as_ref()
            .is_some_and(|b| b.scoped_range(self.layout()).is_some())
    }
}

#[derive(Clone, Debug)]
pub(crate) enum InputShape {
    Owned(OwnedShape),
    Borrowed(Shape),
}
impl InputShape {
    pub(crate) fn of(ty: &Type) -> Result<Self, Error> {
        if let Some(shape) = Shape::of(ty) {
            Ok(Self::Borrowed(shape))
        } else {
            OwnedShape::of(ty).map(Self::Owned)
        }
    }
    pub(crate) fn inputs(&self) -> usize {
        match self {
            Self::Owned(s) => s.inputs(),
            Self::Borrowed(_) => 1,
        }
    }
}

pub(crate) struct Scope {
    parent: u64,
    pub(crate) identity: u64,
    _root: Storage,
    _code: Arc<crate::vm::parallel::Code>,
    _charge: Option<Lease>,
}
impl Scope {
    pub(crate) fn new(identity: u64) -> Result<Arc<Self>, Error> {
        Ok(Arc::new(Self {
            parent: pool::identity().ok_or_else(invalid)?,
            identity,
            _root: super::current().ok_or_else(invalid)?,
            _code: pool::code().ok_or_else(invalid)?,
            _charge: super::reserve_current(
                std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>(),
            )?,
        }))
    }
}
struct Range {
    data: NonNull<u8>,
    len: usize,
    element: Layout,
    scope: Arc<Scope>,
    _charge: Option<Lease>,
}
// SAFETY: capture requires compiler-certified shared scalar storage owned by
// this parent task. Local cancellation drains this subtree before dropping its frames.
// The static group and worker-root failure barrier retain the source owner until
// every reader is cleaned. Only payload bytes cross; Buffer/Rc/frame metadata do
// not. install creates a new nonowning header on the receiving worker.
unsafe impl Send for Range {}

pub(crate) struct Borrowed {
    range: Range,
}
pub(crate) enum Input {
    Owned(Box<Packet>),
    Borrowed(Borrowed),
}
impl Input {
    pub(crate) fn owned(packet: Packet) -> Self {
        // Packet already charges its storage, including this boxed body.
        Self::Owned(Box::new(packet))
    }
    pub(crate) unsafe fn borrow(shape: Shape, word: u64, scope: Arc<Scope>) -> Result<Self, Error> {
        if pool::identity() != Some(scope.parent) {
            return Err(invalid());
        }
        let (data, len) = shape
            .buffer(word)
            .as_ref()
            .and_then(|b| b.scoped_range(shape.layout()))
            .ok_or_else(invalid)?;
        #[cfg(test)]
        observe(&scope, data.as_ptr() as usize, true);
        let range = Range {
            data,
            len,
            element: shape.layout(),
            scope,
            _charge: super::reserve_current(std::mem::size_of::<Borrowed>())?,
        };
        Ok(Self::Borrowed(Borrowed { range }))
    }
    pub(crate) fn inputs(&self) -> usize {
        match self {
            Self::Owned(p) => p.shape.inputs(),
            Self::Borrowed(_) => 1,
        }
    }
    pub(crate) fn install(
        self,
        arena: &mut Arena,
        cleanups: &mut Cleanups,
        base: usize,
        loans: &mut Vec<Arc<Scope>>,
    ) -> Result<Vec<u64>, Error> {
        match self {
            Self::Owned(packet) => packet.install(arena, cleanups, base, None),
            Self::Borrowed(borrowed) => {
                let range = borrowed.range;
                if pool::parent_identity() != Some(range.scope.parent) || range.scope.identity == 0
                {
                    return Err(invalid());
                }
                // Keep the scope through execution AND all local view cleanup.
                loans.push(range.scope.clone());
                let view =
                    unsafe { Buffer::scoped_view(arena, range.data, range.len, range.element)? };
                let word = view.as_ptr() as u64;
                #[cfg(test)]
                observe(&range.scope, range.data.as_ptr() as usize, false);
                Ok(vec![word])
            }
        }
    }
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct Observation {
    pub(crate) parent: u64,
    pub(crate) task: u64,
    pub(crate) scope: u64,
    pub(crate) data: usize,
    pub(crate) thread: std::thread::ThreadId,
    pub(crate) source: bool,
}
#[cfg(test)]
static OBSERVATIONS: std::sync::Mutex<Vec<Observation>> = std::sync::Mutex::new(Vec::new());
#[cfg(test)]
fn observe(scope: &Scope, data: usize, source: bool) {
    OBSERVATIONS.lock().unwrap().push(Observation {
        parent: scope.parent,
        task: pool::identity().unwrap(),
        scope: scope.identity,
        data,
        source,
        thread: std::thread::current().id(),
    });
}
#[cfg(test)]
pub(crate) fn observations(parent: u64) -> Vec<Observation> {
    OBSERVATIONS
        .lock()
        .unwrap()
        .iter()
        .filter(|o| o.parent == parent)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scoped_borrow_shapes_keep_packets_mutation_and_nominal_capabilities_closed() {
        for target in [
            Type::String,
            Type::BufferSlice(Arc::new(Type::Int32)),
            Type::BufferSlice(Arc::new(Type::Bool)),
        ] {
            let ty = Type::reference(target.clone(), false);
            let shape = Shape::of(&ty).unwrap();
            let mut words = Vec::new();
            shape.encode(&mut words);
            assert_eq!(Shape::decode(&words).unwrap().layout(), shape.layout());
            assert!(OwnedShape::of(&ty).is_err());
            assert!(Shape::of(&Type::reference(target, true)).is_none());
        }
        for target in [
            Type::Int32,
            Type::BufferSlice(Arc::new(Type::String)),
            Type::BufferSlice(Arc::new(Type::Named("domain.Handle".into()))),
            Type::BufferSlice(Arc::new(Type::Named("admission.Token".into()))),
        ] {
            assert!(Shape::of(&Type::reference(target, false)).is_none());
        }
        for words in [vec![], vec![99], vec![99, 0], vec![99, 13], vec![99, 1, 0]] {
            assert!(Shape::decode(&words).is_err());
        }
    }
}
