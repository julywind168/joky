//! Detached cleanup storage contains only owned bytes and immutable code evidence.
use super::{packet::invalid, pool::CleanupReceipt, Lease, Storage};
use crate::{
    runtime::{
        arena::Arena,
        buffer::{Buffer, ElementCallback, ElementCleanup, ElementHook},
        cleanup::{Entry, PortableCallback},
    },
    Error,
};

struct Hook {
    before: usize,
    offset: usize,
    indirect: bool,
    guards: Vec<(usize, i32)>,
    callback: PortableCallback,
}
struct Descriptor {
    owners: Vec<usize>,
    hooks: Vec<Hook>,
}
impl Descriptor {
    fn freeze(source: &ElementCleanup) -> Result<Self, Error> {
        let hooks = source
            .hooks
            .iter()
            .map(|h| {
                Ok(Hook {
                    before: h.before,
                    offset: h.offset,
                    indirect: h.indirect,
                    guards: h.guards.clone(),
                    callback: h.callback.portable().ok_or_else(invalid)?,
                })
            })
            .collect::<Result<_, Error>>()?;
        Ok(Self {
            owners: source.owners.clone(),
            hooks,
        })
    }
    fn thaw(&self) -> ElementCleanup {
        ElementCleanup {
            owners: self.owners.clone(),
            hooks: self
                .hooks
                .iter()
                .map(|h| ElementHook {
                    before: h.before,
                    offset: h.offset,
                    indirect: h.indirect,
                    guards: h.guards.clone(),
                    callback: ElementCallback::Portable(h.callback.clone()),
                })
                .collect(),
        }
    }
}

/// Pointer lanes address exclusively owned, validated allocations in `root`.
/// Buffer descriptors contain no local callback/Rc; disposal always runs under
/// a live worker receipt, which also retains native code until cleanup finishes.
struct Value {
    bytes: Vec<u64>,
    descriptor: Descriptor,
    root: Storage,
    _charge: Lease,
}

pub(crate) struct Payload {
    value: Option<Value>,
    receipt: Option<CleanupReceipt>,
}
impl Payload {
    /// The complete entry and all owner lanes are live on the calling worker.
    pub(crate) unsafe fn needed(entry: &Entry, arena: &Arena) -> Result<bool, Error> {
        if !entry.hooks.is_empty()
            || entry
                .storage
                .as_ref()
                .is_some_and(|(_, d)| !d.hooks.is_empty())
        {
            return Ok(true);
        }
        let owners = if let Some((data, d)) = &entry.storage {
            d.owners
                .iter()
                .filter(|&&o| o != usize::MAX)
                .map(|&o| data.add(o).cast::<*mut Buffer>().read_unaligned())
                .collect()
        } else {
            entry.owners.iter().flatten().copied().collect::<Vec<_>>()
        };
        Buffer::has_user_cleanup(arena, owners)
    }

    /// Preparation borrows cleanup responsibility until success. The caller
    /// then relinquishes its entry without executing it, with no fallible gap.
    pub(crate) unsafe fn capture(
        entry: &Entry,
        arena: &Arena,
        returned: bool,
    ) -> Result<Self, Error> {
        let root = arena.parallel_root().ok_or_else(invalid)?.clone();
        let (bytes, descriptor) = if let Some((data, descriptor)) = &entry.storage {
            let mut bytes = vec![0u64; entry.storage_size.div_ceil(8)];
            std::ptr::copy_nonoverlapping(*data, bytes.as_mut_ptr().cast(), entry.storage_size);
            (bytes, Descriptor::freeze(descriptor)?)
        } else {
            if !entry.hooks.is_empty() {
                return Err(invalid());
            }
            let bytes = entry
                .owners
                .iter()
                .map(|o| o.map_or(0, |p| p as u64))
                .collect::<Vec<_>>();
            let descriptor = Descriptor {
                owners: (0..bytes.len()).map(|i| i * 8).collect(),
                hooks: vec![],
            };
            (bytes, descriptor)
        };
        for &offset in descriptor.owners.iter().filter(|&&o| o != usize::MAX) {
            let owner = bytes
                .as_ptr()
                .cast::<u8>()
                .add(offset)
                .cast::<*mut Buffer>()
                .read_unaligned();
            if !owner.is_null() {
                Buffer::validate_parallel_value(arena, owner, true)?;
            }
        }
        let metadata = descriptor.thaw().metadata_bytes().ok_or_else(invalid)?;
        let charge = root
            .accounts()
            .reserve(std::mem::size_of::<Self>() + bytes.len() * 8 + metadata)?;
        let receipt = CleanupReceipt::new(returned)?;
        Ok(Self {
            value: Some(Value {
                bytes,
                descriptor,
                root,
                _charge: charge,
            }),
            receipt: Some(receipt),
        })
    }

    pub(crate) fn install(mut self, arena: &mut Arena) -> Result<Entry, Error> {
        let value = self.value.as_ref().unwrap();
        if !arena
            .parallel_root()
            .is_some_and(|root| root.same_root(&value.root))
        {
            return Err(invalid());
        }
        let size = value.bytes.len() * 8;
        let data = arena.allocate_local(size.max(1), 8)?;
        unsafe {
            std::ptr::copy_nonoverlapping(value.bytes.as_ptr().cast::<u8>(), data.as_ptr(), size);
        }
        let descriptor = value.descriptor.thaw();
        self.value.take();
        self.receipt.take();
        Ok(Entry {
            keep_alive: None,
            storage_size: size,
            storage: Some((data.as_ptr(), descriptor)),
            owners: Default::default(),
            hooks: vec![],
        })
    }
}
impl Drop for Payload {
    fn drop(&mut self) {
        if let Some(mut value) = self.value.take() {
            self.receipt.take().unwrap().defer(Box::new(move || {
                let mut arena = Arena::with_storage(value.root.clone());
                unsafe {
                    value
                        .descriptor
                        .thaw()
                        .run(value.bytes.as_mut_ptr().cast(), &mut arena)
                }
            }));
        }
    }
}
