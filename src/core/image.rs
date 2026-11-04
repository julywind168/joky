//! Private wire format shared by the host build script and its target compiler.
//! Only bytes produced by this build may be read. No user AST deserialization API.
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};

#[cfg(joky_bootstrap)]
#[derive(Default)]
pub(crate) struct Writer {
    pub bytes: Vec<u8>,
    shared: HashMap<(std::any::TypeId, usize), u32>,
}

#[cfg(not(joky_bootstrap))]
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    shared: Vec<Box<dyn std::any::Any>>,
}

#[cfg(not(joky_bootstrap))]
impl<'a> Reader<'a> {
    pub fn decode<T: Image>(bytes: &'a [u8]) -> T {
        let mut input = Self {
            bytes,
            shared: Vec::new(),
        };
        let value = T::read(&mut input);
        assert!(input.bytes.is_empty(), "unused embedded image bytes");
        value
    }

    fn take(&mut self, count: usize) -> &'a [u8] {
        let (head, tail) = self.bytes.split_at(count);
        self.bytes = tail;
        head
    }
}

pub(crate) trait Image: Sized {
    #[cfg(joky_bootstrap)]
    fn write(&self, output: &mut Writer);
    #[cfg(not(joky_bootstrap))]
    fn read(input: &mut Reader<'_>) -> Self;
}

macro_rules! codec {
    (|$value:ident, $out:ident| $write:expr, |$input:ident| $read:expr) => {
        #[cfg(joky_bootstrap)]
        fn write(&self, $out: &mut Writer) {
            let $value = self;
            $write
        }
        #[cfg(not(joky_bootstrap))]
        fn read($input: &mut Reader<'_>) -> Self {
            $read
        }
    };
}

macro_rules! numbers {
    ($($ty:ty),*) => { $(impl Image for $ty {
        codec!(|v, out| out.bytes.extend(v.to_le_bytes()),
            |input| Self::from_le_bytes(input.take(size_of::<Self>()).try_into().unwrap()));
    })* };
}
numbers!(u8, u16, u32, u64, i8, i16, i32, i64, f32, f64);

impl Image for usize {
    codec!(|v, out| u64::try_from(*v).unwrap().write(out), |input| {
        u64::read(input).try_into().unwrap()
    });
}
impl Image for bool {
    codec!(|v, out| u8::from(*v).write(out), |input| u8::read(input)
        != 0);
}
impl Image for std::num::NonZeroU64 {
    codec!(|v, out| v.get().write(out), |input| Self::new(u64::read(
        input
    ))
    .unwrap());
}
impl Image for String {
    codec!(
        |v, out| {
            v.len().write(out);
            out.bytes.extend(v.as_bytes());
        },
        |input| {
            let len = usize::read(input);
            std::str::from_utf8(input.take(len)).unwrap().to_owned()
        }
    );
}
impl Image for Arc<str> {
    codec!(
        |v, out| {
            v.len().write(out);
            out.bytes.extend(v.as_bytes());
        },
        |input| String::read(input).into()
    );
}

impl<T: Image + 'static> Image for Arc<T> {
    codec!(
        |v, out| {
            // References point back to completed nodes in the immutable DAG.
            let key = (std::any::TypeId::of::<T>(), Arc::as_ptr(v) as usize);
            if let Some(&index) = out.shared.get(&key) {
                index.write(out);
            } else {
                u32::MAX.write(out);
                v.as_ref().write(out);
                out.shared
                    .insert(key, u32::try_from(out.shared.len()).unwrap());
            }
        },
        |input| {
            let index = u32::read(input);
            if index == u32::MAX {
                let node = Arc::new(T::read(input));
                input.shared.push(Box::new(node.clone()));
                node
            } else {
                input.shared[index as usize]
                    .downcast_ref::<Self>()
                    .unwrap()
                    .clone()
            }
        }
    );
}
impl<T: Image> Image for Box<T> {
    codec!(|v, out| v.as_ref().write(out), |input| Box::new(T::read(
        input
    )));
}
impl<T: Image> Image for Option<T> {
    codec!(
        |v, out| {
            v.is_some().write(out);
            if let Some(item) = v {
                item.write(out);
            }
        },
        |input| bool::read(input).then(|| T::read(input))
    );
}
impl<A: Image, B: Image> Image for (A, B) {
    codec!(
        |v, out| {
            v.0.write(out);
            v.1.write(out);
        },
        |input| (A::read(input), B::read(input))
    );
}
impl<T: Image> Image for Vec<T> {
    codec!(
        |v, out| {
            v.len().write(out);
            for item in v {
                item.write(out);
            }
        },
        |input| (0..usize::read(input)).map(|_| T::read(input)).collect()
    );
}
impl<T: Image + Ord> Image for BTreeSet<T> {
    codec!(
        |v, out| {
            v.len().write(out);
            for item in v {
                item.write(out);
            }
        },
        |input| (0..usize::read(input)).map(|_| T::read(input)).collect()
    );
}
impl<K: Image + Ord, V: Image> Image for BTreeMap<K, V> {
    codec!(
        |v, out| {
            v.len().write(out);
            for (key, item) in v {
                key.write(out);
                item.write(out);
            }
        },
        |input| (0..usize::read(input))
            .map(|_| (K::read(input), V::read(input)))
            .collect()
    );
}
impl<K: Image + Ord + std::hash::Hash, V: Image> Image for HashMap<K, V> {
    codec!(
        |v, out| {
            v.len().write(out);
            let mut sorted: Vec<_> = v.iter().collect();
            sorted.sort_unstable_by_key(|(key, _)| *key);
            for (key, item) in sorted {
                key.write(out);
                item.write(out);
            }
        },
        |input| (0..usize::read(input))
            .map(|_| (K::read(input), V::read(input)))
            .collect()
    );
}

// Image IDs are assigned by the build-host compiler in a fresh process.
// Start dynamic source and declaration IDs past its high-water mark.
#[cfg(joky_bootstrap)]
pub(crate) const FIRST_DYNAMIC_ID: u64 = 1;
#[cfg(not(joky_bootstrap))]
pub(crate) const FIRST_DYNAMIC_ID: u64 = include!(concat!(env!("OUT_DIR"), "/stdlib-id-base.rs"));
