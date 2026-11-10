//! Activation-owned cleanup entries. Moving a value transfers its registration;
//! removing an entry precedes callbacks so reentrant failure cannot run it twice.
use super::{arena::Arena, buffer::Buffer};
use crate::Error;
use std::collections::BTreeMap;

mod owners;
use owners::Owners;

pub(crate) struct Entry {
    /// Interpreter storage must outlive a transferred descriptor, after the source activation ends.
    pub keep_alive: Option<std::rc::Rc<std::cell::RefCell<Arena>>>,
    /// Complete inline layout, retained when a field descriptor is detached.
    pub storage_size: usize,
    /// Borrowed storage is read when cleanup runs, using the current owning lanes.
    pub storage: Option<(*mut u8, super::buffer::ElementCleanup)>,
    // Handle-only entries have no inline storage.
    pub owners: Owners,
}

impl Entry {
    pub(crate) fn materialize(&mut self) {
        if let Some((data, descriptor)) = self.storage.take() {
            // Registration guarantees live storage and matching descriptors.
            let keep_alive = self.keep_alive.take();
            *self = unsafe { descriptor.snapshot(data) };
            self.keep_alive = keep_alive;
        }
    }

    /// Release child owners, continuing after the first error.
    ///
    /// # Safety
    /// Child handles are unique live owners in arena. Descriptor storage remains
    /// alive for the call on the task's execution thread.
    pub(crate) unsafe fn run(mut self, arena: &mut Arena) -> Result<(), Error> {
        if let Some((data, descriptor)) = self.storage.take() {
            return descriptor.run(data, arena);
        }
        let mut error = None;
        for owner in self.owners.into_iter().flatten() {
            if let Err(next) = Buffer::release(arena, owner) {
                error.get_or_insert(next);
            }
        }
        error.map_or(Ok(()), Err)
    }
}

// The first live entry needs no tree node. Extra simultaneous entries retain
// logarithmic lookup; declaration order remains exclusively in Cleanups::order.
#[derive(Default)]
struct Entries {
    first: Option<(usize, Entry)>,
    rest: BTreeMap<usize, Entry>,
}
impl Entries {
    fn get_mut(&mut self, slot: &usize) -> Option<&mut Entry> {
        if let Some((key, entry)) = &mut self.first {
            if key == slot {
                return Some(entry);
            }
        }
        self.rest.get_mut(slot)
    }
    fn insert(&mut self, slot: usize, entry: Entry) {
        if let Some(previous) = self.get_mut(&slot) {
            *previous = entry;
        } else if self.first.is_none() {
            self.first = Some((slot, entry));
        } else {
            self.rest.insert(slot, entry);
        }
    }
    fn remove(&mut self, slot: &usize) -> Option<Entry> {
        if self.first.as_ref().is_some_and(|(key, _)| key == slot) {
            self.first.take().map(|(_, entry)| entry)
        } else {
            self.rest.remove(slot)
        }
    }
}

#[derive(Default)]
pub(crate) struct Cleanups {
    pub(crate) places: super::place::Places,
    entries: Entries,
    order: Vec<usize>,
}

impl Cleanups {
    /// Pin an independent capture before its parent's reusable local changes.
    /// Failure leaves the original cleanup registration intact.
    pub(crate) fn snapshot(&mut self, slot: usize) -> Result<(), Error> {
        let Some(entry) = self.entries.get_mut(&slot) else {
            return Ok(());
        };
        let Some((data, _)) = entry.storage.as_mut() else {
            return Ok(());
        };
        let mut storage = Arena::default();
        let copy = storage.allocate_local(entry.storage_size, std::mem::align_of::<u64>())?;
        // SAFETY: The registered layout and source remain live until this transfer.
        unsafe { std::ptr::copy_nonoverlapping(*data, copy.as_ptr(), entry.storage_size) };
        *data = copy.as_ptr();
        entry.keep_alive = Some(std::rc::Rc::new(std::cell::RefCell::new(storage)));
        Ok(())
    }

    pub(crate) fn insert(&mut self, slot: usize, entry: Entry) {
        // Remember a binding's original position across moves/reinitialization.
        // Scope exits use explicit End operations; failure/cancellation must
        // preserve the same declaration order, not the latest assignment order.
        if !self.order.contains(&slot) {
            self.order.push(slot);
        }
        self.entries.insert(slot, entry);
    }

    pub(crate) fn take(&mut self, slot: usize) -> Option<Entry> {
        self.entries.remove(&slot)
    }

    /// Release a complete owner's buffers, even after failure.
    ///
    /// # Safety
    /// Entries must hold unique live buffers from this arena. Descriptor storage
    /// must remain live until removal.
    pub(crate) unsafe fn run(&mut self, slot: usize, arena: &mut Arena) -> Result<(), Error> {
        let Some(entry) = self.entries.remove(&slot) else {
            return Ok(());
        };
        entry.run(arena)
    }

    /// Detach complete owning subtrees with disjoint cleanup responsibility.
    pub(crate) fn take_fields(&mut self, slot: usize, first: usize, count: usize) -> Entry {
        let entry = self.entries.get_mut(&slot).expect("registered owner");
        if let Some((data, descriptor)) = &mut entry.storage {
            let selected = super::buffer::ElementCleanup {
                owners: descriptor.owners[first..first + count].to_vec(),
            };
            descriptor.owners[first..first + count].fill(usize::MAX);
            return Entry {
                keep_alive: entry.keep_alive.clone(),
                storage_size: entry.storage_size,
                storage: Some((*data, selected)),
                owners: Default::default(),
            };
        }
        let owners = entry.owners[first..first + count]
            .iter_mut()
            .map(Option::take)
            .collect();
        Entry {
            keep_alive: None,
            storage_size: 0,
            storage: None,
            owners,
        }
    }

    /// Replace a complete field, preserving the source if old-field cleanup fails.
    ///
    /// # Safety
    /// Both groups must satisfy the callback and unique-buffer requirements of run.
    pub(crate) unsafe fn replace_fields(
        &mut self,
        slot: usize,
        first: usize,
        count: usize,
        src: usize,
        arena: &mut Arena,
    ) -> Result<(), Error> {
        let storage = self.entries.get_mut(&slot).unwrap().storage.clone();
        self.take_fields(slot, first, count).run(arena)?;
        let mut next = self.take(src).expect("replacement owner");
        if let Some(storage) = storage {
            // The following checked store installs the source bits atomically;
            // retain the original field layout, including its enclosing guards.
            self.entries.get_mut(&slot).unwrap().storage = Some(storage);
            return Ok(());
        }
        next.materialize();
        let entry = self.entries.get_mut(&slot).unwrap();
        assert_eq!(next.owners.len(), count);
        for (destination, source) in entry.owners[first..first + count]
            .iter_mut()
            .zip(next.owners)
        {
            *destination = source;
        }
        Ok(())
    }

    /// End a value, including its initialized fields if construction diverged.
    ///
    /// # Safety
    /// Entries in the slot range must satisfy the requirements of run.
    pub(crate) unsafe fn end(
        &mut self,
        start: usize,
        width: usize,
        arena: &mut Arena,
    ) -> Result<(), Error> {
        if width == 1 {
            return self.run(start, arena);
        }
        let mut error = None;
        // run removes entries but leaves declaration order intact. Copy each key
        // before callbacks; no temporary list or entry borrow spans resource release.
        for index in (0..self.order.len()).rev() {
            let slot = self.order[index];
            if (start..start + width).contains(&slot) {
                if let Err(next) = self.run(slot, arena) {
                    error.get_or_insert(next);
                }
            }
        }
        error.map_or(Ok(()), Err)
    }

    /// Drain live entries in reverse registration order.
    ///
    /// # Safety
    /// Every entry must satisfy the same requirements as `run`.
    pub(crate) unsafe fn finish(&mut self, arena: &mut Arena) -> Result<(), Error> {
        let mut error = None;
        while let Some(slot) = self.order.pop() {
            if let Err(next) = self.run(slot, arena) {
                error.get_or_insert(next);
            }
        }
        if let Err(next) = self.places.finish(arena) {
            error.get_or_insert(next);
        }
        error.map_or(Ok(()), Err)
    }
}

pub(crate) struct NativeFrame {
    pub(crate) arena: *mut Arena,
    pub(crate) entries: Cleanups,
}

#[export_name = "__joky_cleanup_new"]
pub(crate) unsafe extern "C" fn new(arena: *mut Arena) -> *mut NativeFrame {
    Box::into_raw(Box::new(NativeFrame {
        arena,
        entries: Cleanups::default(),
    }))
}

#[export_name = "__joky_cleanup_register"]
unsafe extern "C" fn register(frame: *mut NativeFrame, slot: usize, owner: *mut Buffer) {
    (*frame).entries.insert(
        slot,
        Entry {
            keep_alive: None,
            storage_size: 0,
            storage: None,
            owners: [std::ptr::NonNull::new(owner).map(std::ptr::NonNull::as_ptr)].into(),
        },
    );
}

#[export_name = "__joky_cleanup_append"]
unsafe extern "C" fn append(
    frame: *mut NativeFrame,
    slot: usize,
    field: usize,
    owner: *mut Buffer,
) {
    (*frame).entries.take(field);
    (*frame)
        .entries
        .entries
        .get_mut(&slot)
        .unwrap()
        .owners
        .push(std::ptr::NonNull::new(owner).map(std::ptr::NonNull::as_ptr));
}

#[export_name = "__joky_cleanup_forget"]
unsafe extern "C" fn forget(frame: *mut NativeFrame, slot: usize) {
    (*frame).entries.take(slot);
}

#[export_name = "__joky_cleanup_extract_field"]
unsafe extern "C" fn extract_field(
    frame: *mut NativeFrame,
    dst: usize,
    src: usize,
    first: usize,
    count: usize,
) {
    let entry = (*frame).entries.take_fields(src, first, count);
    (*frame).entries.insert(dst, entry);
}

#[export_name = "__joky_cleanup_replace_field"]
unsafe extern "C" fn replace_field(
    frame: *mut NativeFrame,
    slot: usize,
    first: usize,
    count: usize,
    src: usize,
) {
    if let Err(error) =
        (*frame)
            .entries
            .replace_fields(slot, first, count, src, &mut *(*frame).arena)
    {
        super::error::record_error(error);
    }
}

#[export_name = "__joky_cleanup_move"]
unsafe extern "C" fn move_entry(frame: *mut NativeFrame, dst: usize, src: usize) {
    if dst == src {
        return;
    }
    if let Err(error) = (*frame).entries.run(dst, &mut *(*frame).arena) {
        super::error::record_error(error);
        return;
    }
    if let Some(entry) = (*frame).entries.take(src) {
        (*frame).entries.insert(dst, entry);
    }
}

#[export_name = "__joky_cleanup_run"]
unsafe extern "C" fn run(frame: *mut NativeFrame, slot: usize, width: usize) {
    if let Err(error) = (*frame).entries.end(slot, width, &mut *(*frame).arena) {
        super::error::record_error(error);
    }
}

#[export_name = "__joky_cleanup_finish"]
pub(crate) unsafe extern "C" fn finish(frame: *mut NativeFrame) {
    let mut frame = Box::from_raw(frame);
    if let Err(error) = frame.entries.finish(&mut *frame.arena) {
        super::error::record_error(error);
    }
}

/// An owning return is published only after local cleanup completes.
/// Keep its cleanup responsibility until that point, including native failures.
#[export_name = "__joky_cleanup_finish_return"]
pub(crate) unsafe extern "C" fn finish_return(frame: *mut NativeFrame, slot: usize) {
    take_return(frame, slot);
}

/// Detach a successful return while retaining its complete cleanup description.
/// The caller assumes the uniquely owned result and its live storage on success.
pub(crate) unsafe fn take_return(frame: *mut NativeFrame, slot: usize) -> Option<Entry> {
    let mut frame = Box::from_raw(frame);
    let returned = frame.entries.take(slot);
    if let Err(error) = frame.entries.finish(&mut *frame.arena) {
        if let Some(returned) = returned {
            let _ = returned.run(&mut *frame.arena);
        }
        super::error::record_error(error);
        None
    } else {
        returned
    }
}

// Compiler-generated callers supply live exclusive owner storage and matching
// layouts. These descriptors remain on the serialized task execution thread.
#[export_name = "__joky_cleanup_storage_v2"]
unsafe extern "C" fn storage(frame: *mut NativeFrame, slot: usize, data: *mut u8, size: usize) {
    let entry = (*frame).entries.entries.get_mut(&slot).unwrap();
    entry.storage = Some((data, Default::default()));
    entry.storage_size = size;
}
#[export_name = "__joky_cleanup_snapshot"]
unsafe extern "C" fn snapshot(frame: *mut NativeFrame, slot: usize) {
    if let Err(error) = (*frame).entries.snapshot(slot) {
        super::error::record_error(error);
    }
}
#[export_name = "__joky_cleanup_storage_owner"]
unsafe extern "C" fn storage_owner(frame: *mut NativeFrame, slot: usize, offset: usize) {
    (*frame)
        .entries
        .entries
        .get_mut(&slot)
        .unwrap()
        .storage
        .as_mut()
        .unwrap()
        .1
        .owners
        .push(offset);
}

#[export_name = "__joky_memory_swap"]
unsafe extern "C" fn swap(left: *mut u8, right: *mut u8, size: usize) {
    // Disjoint live capabilities and their layout are proven by the IR.
    std::ptr::swap_nonoverlapping(left, right, size);
}

#[export_name = "__joky_buffer_swap"]
unsafe extern "C" fn swap_buffer(left: *mut Buffer, right: *mut Buffer) {
    // Matching exclusive buffer capabilities keep their stable handle identities.
    Buffer::swap(left, right);
}

pub(crate) fn symbols() -> [(&'static str, *const u8); 15] {
    [
        ("__joky_memory_swap", swap as *const u8),
        ("__joky_buffer_swap", swap_buffer as *const u8),
        ("__joky_cleanup_storage_v2", storage as *const u8),
        ("__joky_cleanup_snapshot", snapshot as *const u8),
        ("__joky_cleanup_storage_owner", storage_owner as *const u8),
        ("__joky_cleanup_new", new as *const u8),
        ("__joky_cleanup_register", register as *const u8),
        ("__joky_cleanup_append", append as *const u8),
        ("__joky_cleanup_forget", forget as *const u8),
        ("__joky_cleanup_extract_field", extract_field as *const u8),
        ("__joky_cleanup_replace_field", replace_field as *const u8),
        ("__joky_cleanup_move", move_entry as *const u8),
        ("__joky_cleanup_run", run as *const u8),
        ("__joky_cleanup_finish", finish as *const u8),
        ("__joky_cleanup_finish_return", finish_return as *const u8),
    ]
}

#[cfg(test)]
mod tests;
