//! Activation-owned cleanup entries. Moving a value transfers its registration;
//! removing an entry precedes callbacks so reentrant failure cannot run it twice.
use super::{arena::Arena, buffer::Buffer};
use crate::Error;
use std::collections::BTreeMap;

pub(crate) type Callback = unsafe extern "C" fn(*mut u8, *mut Arena);

pub(crate) type InterpretedCallback = std::rc::Rc<dyn Fn(*mut u8, &mut Arena) -> Result<(), Error>>;

pub(crate) enum Hook {
    Native {
        data: *mut u8,
        callback: Callback,
    },
    Interpreted {
        data: *mut u8,
        callback: InterpretedCallback,
    },
}

// Hooks may create new owners whose cleanup invokes more hooks. Bound this
// runtime reentrancy independently of the language call stack on both backends.
thread_local! {
    static DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
struct Depth;
impl Drop for Depth {
    fn drop(&mut self) {
        DEPTH.with(|depth| depth.set(depth.get() - 1));
    }
}
impl Hook {
    pub(crate) unsafe fn run(self, arena: &mut Arena) -> Result<(), Error> {
        let _cleaning = super::parallel::pool::CleanupGuard::enter();
        let entered = DEPTH.with(|depth| {
            if depth.get() >= 32 {
                false
            } else {
                depth.set(depth.get() + 1);
                true
            }
        });
        if !entered {
            return Err(Error::new_with_code(
                crate::ErrorPhase::Runtime,
                crate::ErrorCode::ResourceLimit,
                "Destructor nesting limit exceeded",
            ));
        }
        let _depth = Depth;
        match self {
            Self::Native { data, callback } => super::error::checked(|| callback(data, arena)),
            Self::Interpreted { data, callback } => callback(data, arena),
        }
    }
}

pub(crate) struct Entry {
    /// Interpreter storage must outlive a transferred descriptor, even without hooks.
    pub keep_alive: Option<std::rc::Rc<std::cell::RefCell<Arena>>>,
    /// Complete inline layout, retained when a field descriptor is detached.
    pub storage_size: usize,
    /// Borrowed storage is read when cleanup runs, after enclosing Drop hooks.
    pub storage: Option<(*mut u8, super::buffer::ElementCleanup)>,
    // Handle-only entries have no inline storage or source-defined Drop hooks.
    pub owners: Vec<Option<*mut Buffer>>,
    pub hooks: Vec<(usize, Hook)>,
}

impl Entry {
    fn materialize(&mut self) {
        if let Some((data, descriptor)) = self.storage.take() {
            // Registration guarantees live storage and matching descriptors.
            let keep_alive = self.keep_alive.take();
            *self = unsafe { descriptor.snapshot(data) };
            self.keep_alive = keep_alive;
        }
    }

    /// Run hooks and release child owners, continuing after the first error.
    ///
    /// # Safety
    /// Child handles are unique live owners in arena. Hook bytes and callback
    /// code remain alive for the call on the task's execution thread.
    pub(crate) unsafe fn run(mut self, arena: &mut Arena) -> Result<(), Error> {
        if let Some((data, descriptor)) = self.storage.take() {
            return descriptor.run(data, arena);
        }
        let mut error = None;
        let mut hooks = self.hooks.into_iter().peekable();
        for (index, owner) in self.owners.into_iter().enumerate() {
            while hooks.peek().is_some_and(|(before, _)| *before == index) {
                if let Err(next) = hooks.next().unwrap().1.run(arena) {
                    error.get_or_insert(next);
                }
            }
            if let Some(owner) = owner {
                if let Err(next) = Buffer::release(arena, owner) {
                    error.get_or_insert(next);
                }
            }
        }
        debug_assert!(hooks.next().is_none());
        error.map_or(Ok(()), Err)
    }
}

#[derive(Default)]
pub(crate) struct Cleanups {
    pub(crate) places: super::place::Places,
    entries: BTreeMap<usize, Entry>,
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

    /// Run a complete owner's hooks and release all buffers, even after failure.
    ///
    /// # Safety
    /// Entries must hold unique live buffers from this arena. Callback code and
    /// data must remain live until removal, with the declared destructor ABI.
    pub(crate) unsafe fn run(&mut self, slot: usize, arena: &mut Arena) -> Result<(), Error> {
        let Some(entry) = self.entries.remove(&slot) else {
            return Ok(());
        };
        entry.run(arena)
    }

    /// Detach complete owning subtrees; Drop ancestors are rejected by the IR.
    pub(crate) fn take_fields(&mut self, slot: usize, first: usize, count: usize) -> Entry {
        let entry = self.entries.get_mut(&slot).expect("registered owner");
        if let Some((data, descriptor)) = &mut entry.storage {
            let mut selected = super::buffer::ElementCleanup {
                owners: descriptor.owners[first..first + count].to_vec(),
                hooks: vec![],
            };
            descriptor.owners[first..first + count].fill(usize::MAX);
            descriptor.hooks.retain(|hook| {
                if (first..first + count).contains(&hook.before) {
                    let mut hook = hook.clone();
                    hook.before -= first;
                    selected.hooks.push(hook);
                    false
                } else {
                    true
                }
            });
            return Entry {
                keep_alive: entry.keep_alive.clone(),
                storage_size: entry.storage_size,
                storage: Some((*data, selected)),
                owners: vec![],
                hooks: vec![],
            };
        }
        let owners = entry.owners[first..first + count]
            .iter_mut()
            .map(Option::take)
            .collect();
        let mut hooks = Vec::new();
        let mut remaining = Vec::new();
        for (index, hook) in entry.hooks.drain(..) {
            if (first..first + count).contains(&index) {
                hooks.push((index - first, hook));
            } else {
                remaining.push((index, hook));
            }
        }
        entry.hooks = remaining;
        Entry {
            keep_alive: None,
            storage_size: 0,
            storage: None,
            owners,
            hooks,
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
        let storage = self.entries[&slot].storage.clone();
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
        entry.hooks.extend(
            next.hooks
                .into_iter()
                .map(|(index, hook)| (first + index, hook)),
        );
        entry.hooks.sort_by_key(|(index, _)| *index);
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
        let slots: Vec<_> = self
            .order
            .iter()
            .rev()
            .copied()
            .filter(|slot| (start..start + width).contains(slot))
            .collect();
        let mut error = None;
        for slot in slots {
            if let Err(next) = self.run(slot, arena) {
                error.get_or_insert(next);
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
        self.places.finish(arena);
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
            owners: vec![std::ptr::NonNull::new(owner).map(std::ptr::NonNull::as_ptr)],
            hooks: Vec::new(),
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

#[export_name = "__joky_cleanup_hook"]
unsafe extern "C" fn hook(
    frame: *mut NativeFrame,
    slot: usize,
    before: usize,
    data: *mut u8,
    callback: Callback,
) {
    (*frame)
        .entries
        .entries
        .get_mut(&slot)
        .unwrap()
        .hooks
        .push((before, Hook::Native { data, callback }));
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

/// An owning return is published only after every local destructor succeeds.
/// Keep its cleanup responsibility until that point, including native failures.
#[export_name = "__joky_cleanup_finish_return"]
pub(crate) unsafe extern "C" fn finish_return(frame: *mut NativeFrame, slot: usize) {
    let mut frame = Box::from_raw(frame);
    let returned = frame.entries.take(slot);
    if let Err(error) = frame.entries.finish(&mut *frame.arena) {
        if let Some(returned) = returned {
            let _ = returned.run(&mut *frame.arena);
        }
        super::error::record_error(error);
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
#[export_name = "__joky_cleanup_storage_hook"]
unsafe extern "C" fn storage_hook(
    frame: *mut NativeFrame,
    slot: usize,
    before: usize,
    offset: usize,
    indirect: usize,
    callback: Callback,
) {
    (*frame)
        .entries
        .entries
        .get_mut(&slot)
        .unwrap()
        .storage
        .as_mut()
        .unwrap()
        .1
        .hooks
        .push(super::buffer::ElementHook {
            before,
            offset,
            indirect: indirect != 0,
            guards: vec![],
            callback: super::buffer::ElementCallback::Native(callback),
        });
}
#[export_name = "__joky_cleanup_storage_guard"]
unsafe extern "C" fn storage_guard(frame: *mut NativeFrame, slot: usize, offset: usize, tag: i32) {
    (*frame)
        .entries
        .entries
        .get_mut(&slot)
        .unwrap()
        .storage
        .as_mut()
        .unwrap()
        .1
        .hooks
        .last_mut()
        .unwrap()
        .guards
        .push((offset, tag));
}
#[export_name = "__joky_memory_swap"]
unsafe extern "C" fn swap(left: *mut u8, right: *mut u8, size: usize) {
    // Disjoint live capabilities and their layout are proven by the IR.
    std::ptr::swap_nonoverlapping(left, right, size);
}

#[export_name = "__joky_buffer_swap"]
unsafe extern "C" fn swap_buffer(left: *mut Buffer, right: *mut Buffer) {
    // Matching exclusive buffer capabilities keep their stable handle identities.
    std::ptr::swap(left, right);
}

pub(crate) fn symbols() -> [(&'static str, *const u8); 18] {
    [
        ("__joky_memory_swap", swap as *const u8),
        ("__joky_buffer_swap", swap_buffer as *const u8),
        ("__joky_cleanup_storage_v2", storage as *const u8),
        ("__joky_cleanup_snapshot", snapshot as *const u8),
        ("__joky_cleanup_storage_owner", storage_owner as *const u8),
        ("__joky_cleanup_storage_hook", storage_hook as *const u8),
        ("__joky_cleanup_storage_guard", storage_guard as *const u8),
        ("__joky_cleanup_new", new as *const u8),
        ("__joky_cleanup_register", register as *const u8),
        ("__joky_cleanup_append", append as *const u8),
        ("__joky_cleanup_hook", hook as *const u8),
        ("__joky_cleanup_forget", forget as *const u8),
        ("__joky_cleanup_extract_field", extract_field as *const u8),
        ("__joky_cleanup_replace_field", replace_field as *const u8),
        ("__joky_cleanup_move", move_entry as *const u8),
        ("__joky_cleanup_run", run as *const u8),
        ("__joky_cleanup_finish", finish as *const u8),
        ("__joky_cleanup_finish_return", finish_return as *const u8),
    ]
}
