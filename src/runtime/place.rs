//! Task-charged storage for compiler-managed Copy values. Physical reclamation
//! is independent of the source language's ownership and Drop capabilities.
use super::arena::Arena;
use crate::{Error, ErrorCode, ErrorPhase};
use std::ptr::NonNull;

/// Relocatable copy program. Child programs follow their parent; `end` is the
/// first node after the entire subtree. All offsets are relative to the parent.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct CopyNode {
    pub offset: u64,
    /// 0: scalar bytes, 1: fields, 2: homogeneous array, 3: tagged enum.
    pub kind: u64,
    /// Scalar byte width, array length, or number of enum variants.
    pub count: u64,
    pub stride: u64,
    pub end: u64,
}

/// Copy logical fields without reading padding or inactive enum payloads.
///
/// # Safety
/// `nodes` must be a compiler-validated, bounded, finite copy program. Source
/// and destination must have its complete layout and valid alignment. Source
/// is fully initialized; destinations are disjoint or exactly identical. The
/// module owning the descriptor must remain alive throughout this call.
pub(crate) unsafe fn copy(dst: *mut u8, src: *const u8, nodes: &[CopyNode]) {
    unsafe fn visit(dst: *mut u8, src: *const u8, nodes: &[CopyNode], index: usize) {
        let node = nodes[index];
        let dst = dst.add(node.offset as usize);
        let src = src.add(node.offset as usize);
        match node.kind {
            0 => std::ptr::copy_nonoverlapping(src, dst, node.count as usize),
            1 => {
                let mut child = index + 1;
                while child < node.end as usize {
                    visit(dst, src, nodes, child);
                    child = nodes[child].end as usize;
                }
            }
            2 => {
                for item in 0..node.count as usize {
                    let offset = item * node.stride as usize;
                    visit(dst.add(offset), src.add(offset), nodes, index + 1);
                }
            }
            3 => {
                let tag = src.cast::<i32>().read_unaligned();
                dst.cast::<i32>().write_unaligned(tag);
                let mut child = index + 1;
                for variant in 0..node.count {
                    if variant == tag as u64 {
                        visit(dst, src, nodes, child);
                        break;
                    }
                    child = nodes[child].end as usize;
                }
            }
            _ => unreachable!("validated copy program"),
        }
    }
    if !std::ptr::eq(dst, src) {
        visit(dst, src, nodes, 0);
    }
}

struct Registration {
    next: Option<NonNull<Registration>>,
    key: usize,
    payload: Option<NonNull<u8>>,
    initialized: u64,
    length: u64,
}

/// Each activation owns a bounded reusable list. Nodes and zero-sized payloads
/// use the same task arena and accounting as ordinary payloads.
#[derive(Default)]
pub(crate) struct Places {
    head: Option<NonNull<Registration>>,
}

impl Places {
    fn find(&self, key: usize) -> Option<NonNull<Registration>> {
        let mut next = self.head;
        while let Some(node) = next {
            // Nodes remain live until finish, which clears head before release.
            let registration = unsafe { node.as_ref() };
            if registration.key == key {
                return Some(node);
            }
            next = registration.next;
        }
        None
    }

    /// Reserve cleanup before allocating or publishing a payload. The caller
    /// supplies a unique compiler slot and the validated concrete object layout.
    pub(crate) fn begin(
        &mut self,
        arena: &mut Arena,
        key: usize,
        size: usize,
        align: usize,
        length: u64,
    ) -> Result<NonNull<u8>, Error> {
        let (mut node, fresh) = match self.find(key) {
            Some(node) => (node, false),
            None => {
                let node = arena
                    .allocate(
                        std::mem::size_of::<Registration>(),
                        std::mem::align_of::<Registration>(),
                    )?
                    .cast::<Registration>();
                unsafe {
                    node.as_ptr().write(Registration {
                        next: self.head,
                        key,
                        payload: None,
                        initialized: 0,
                        length,
                    });
                }
                (node, true)
            }
        };
        if unsafe { node.as_ref() }.payload.is_some() {
            return Err(invalid("Place slot is already live"));
        }
        let payload = match arena.allocate(size, align) {
            Ok(payload) => payload,
            Err(error) => {
                if fresh {
                    unsafe { arena.release(node.cast()) };
                }
                return Err(error);
            }
        };
        // Padding is defined, but zeroing does not initialize any logical value.
        unsafe {
            payload.as_ptr().write_bytes(0, size.max(1));
            let registration = node.as_mut();
            registration.payload = Some(payload);
            registration.initialized = 0;
            registration.length = length;
        }
        if fresh {
            self.head = Some(node);
        }
        Ok(payload)
    }

    /// Commit exactly one complete element after its non-failing typed store.
    pub(crate) fn commit_next(&mut self, key: usize) -> Result<(), Error> {
        let mut node = self
            .find(key)
            .ok_or_else(|| invalid("Unknown place slot"))?;
        let node = unsafe { node.as_mut() };
        if node.payload.is_none() || node.initialized == node.length {
            return Err(invalid("FixedArray initialization exceeds its length"));
        }
        node.initialized += 1;
        Ok(())
    }

    /// Publish only a fully initialized value; this preserves its registration.
    pub(crate) fn publish(&self, key: usize) -> Result<NonNull<u8>, Error> {
        let node = self
            .find(key)
            .ok_or_else(|| invalid("Unknown place slot"))?;
        let node = unsafe { node.as_ref() };
        if node.initialized != node.length {
            return Err(invalid("FixedArray initialization is incomplete"));
        }
        node.payload.ok_or_else(|| invalid("Place slot has ended"))
    }

    /// End a scope's payload while retaining its charged registration for reuse.
    ///
    /// # Safety
    /// No value or reference may access this slot after this call. The arena
    /// must be the same task arena used by begin, and must outlive this registry.
    pub(crate) unsafe fn end(&mut self, arena: &mut Arena, key: usize) {
        if let Some(mut node) = self.find(key) {
            if let Some(payload) = node.as_mut().payload.take() {
                arena.release(payload);
            }
        }
    }

    /// Release every remaining payload and descriptor, including partial values.
    ///
    /// # Safety
    /// All child activations and callbacks using these places have ended. The
    /// arena is the live arena originally used for every registration.
    pub(crate) unsafe fn finish(&mut self, arena: &mut Arena) {
        let mut next = self.head.take();
        while let Some(node) = next {
            let registration = node.as_ref();
            next = registration.next;
            if let Some(payload) = registration.payload {
                arena.release(payload);
            }
            arena.release(node.cast());
        }
    }
}

fn invalid(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, ErrorCode::InvalidState, message)
}

impl Places {
    fn by_pointer(&self, pointer: *mut u8) -> Option<NonNull<Registration>> {
        let mut next = self.head;
        while let Some(node) = next {
            let registration = unsafe { node.as_ref() };
            if registration.payload.is_some_and(|p| p.as_ptr() == pointer) {
                return Some(node);
            }
            next = registration.next;
        }
        None
    }

    pub(crate) fn materialize(
        &mut self,
        arena: &mut Arena,
        key: usize,
        size: usize,
        align: usize,
    ) -> Result<NonNull<u8>, Error> {
        if let Some(pointer) = self.find(key).and_then(|n| unsafe { n.as_ref() }.payload) {
            return Ok(pointer);
        }
        self.begin(arena, key, size, align, 0)
    }

    /// Transfer the registration, never the bytes, of a trusted initialization token.
    pub(crate) fn move_value(
        &mut self,
        arena: &mut Arena,
        key: usize,
        pointer: *mut u8,
        publish: bool,
    ) -> Result<NonNull<u8>, Error> {
        let mut source = self
            .by_pointer(pointer)
            .ok_or_else(|| invalid("Unknown initialization token"))?;
        if publish {
            self.publish(unsafe { source.as_ref() }.key)?;
        }
        if unsafe { source.as_ref() }.key == key {
            return Ok(NonNull::new(pointer).unwrap());
        }
        let mut destination = match self.find(key) {
            Some(node) => node,
            None => {
                let node = arena
                    .allocate(
                        std::mem::size_of::<Registration>(),
                        std::mem::align_of::<Registration>(),
                    )?
                    .cast::<Registration>();
                unsafe {
                    node.as_ptr().write(Registration {
                        next: self.head,
                        key,
                        payload: None,
                        initialized: 0,
                        length: 0,
                    });
                }
                self.head = Some(node);
                node
            }
        };
        unsafe {
            if destination.as_ref().payload.is_some() {
                return Err(invalid("Place destination is already live"));
            }
            let source = source.as_mut();
            let destination = destination.as_mut();
            destination.payload = source.payload.take();
            destination.length = source.length;
            destination.initialized = source.initialized;
        }
        Ok(NonNull::new(pointer).unwrap())
    }

    pub(crate) fn next(&self, pointer: *mut u8, stride: usize) -> Result<*mut u8, Error> {
        let node = self
            .by_pointer(pointer)
            .ok_or_else(|| invalid("Unknown initialization token"))?;
        let node = unsafe { node.as_ref() };
        if node.initialized >= node.length {
            return Err(invalid("FixedArray initialization exceeds its length"));
        }
        Ok(unsafe { pointer.add(node.initialized as usize * stride) })
    }
    pub(crate) fn commit_pointer(&mut self, pointer: *mut u8) -> Result<(), Error> {
        let node = self
            .by_pointer(pointer)
            .ok_or_else(|| invalid("Unknown initialization token"))?;
        self.commit_next(unsafe { node.as_ref() }.key)
    }
}

fn recorded<T: Default>(result: Result<T, Error>) -> T {
    result.unwrap_or_else(|error| {
        super::error::record_error(error);
        T::default()
    })
}
// The private ABI accepts live activation/arena pointers and compiler-validated
// layouts. It runs only on the owning task thread; no pointer crosses a host ABI.
#[export_name = "__joky_place_begin"]
unsafe extern "C" fn native_begin(
    frame: *mut super::cleanup::NativeFrame,
    key: usize,
    size: usize,
    align: usize,
    length: u64,
) -> *mut u8 {
    recorded(
        (*frame)
            .entries
            .places
            .begin(&mut *(*frame).arena, key, size, align, length)
            .map(NonNull::as_ptr),
    )
}
#[export_name = "__joky_place_materialize"]
unsafe extern "C" fn native_materialize(
    frame: *mut super::cleanup::NativeFrame,
    key: usize,
    size: usize,
    align: usize,
) -> *mut u8 {
    recorded(
        (*frame)
            .entries
            .places
            .materialize(&mut *(*frame).arena, key, size, align)
            .map(NonNull::as_ptr),
    )
}
#[export_name = "__joky_place_move"]
unsafe extern "C" fn native_move(
    frame: *mut super::cleanup::NativeFrame,
    key: usize,
    pointer: *mut u8,
    publish: usize,
) -> *mut u8 {
    recorded(
        (*frame)
            .entries
            .places
            .move_value(&mut *(*frame).arena, key, pointer, publish != 0)
            .map(NonNull::as_ptr),
    )
}
#[export_name = "__joky_place_next"]
unsafe extern "C" fn native_next(
    frame: *mut super::cleanup::NativeFrame,
    pointer: *mut u8,
    stride: usize,
) -> *mut u8 {
    recorded((*frame).entries.places.next(pointer, stride))
}
#[export_name = "__joky_place_commit"]
unsafe extern "C" fn native_commit(frame: *mut super::cleanup::NativeFrame, pointer: *mut u8) {
    recorded((*frame).entries.places.commit_pointer(pointer))
}
#[export_name = "__joky_place_end"]
unsafe extern "C" fn native_end(frame: *mut super::cleanup::NativeFrame, key: usize) {
    (*frame).entries.places.end(&mut *(*frame).arena, key);
}
#[export_name = "__joky_place_copy"]
unsafe extern "C" fn native_copy(
    dst: *mut u8,
    src: *const u8,
    nodes: *const CopyNode,
    count: usize,
) {
    copy(dst, src, std::slice::from_raw_parts(nodes, count));
}
pub(crate) fn symbols() -> [(&'static str, *const u8); 7] {
    [
        ("__joky_place_begin", native_begin as *const u8),
        ("__joky_place_materialize", native_materialize as *const u8),
        ("__joky_place_move", native_move as *const u8),
        ("__joky_place_next", native_next as *const u8),
        ("__joky_place_commit", native_commit as *const u8),
        ("__joky_place_end", native_end as *const u8),
        ("__joky_place_copy", native_copy as *const u8),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::arena::Budget;
    use std::{cell::Cell, rc::Rc};

    #[test]
    fn fixed_array_f2a_places_charge_reuse_and_roll_back_before_publication() {
        let budget = Rc::new(Budget {
            bytes: Cell::new(0),
            limit: Cell::new(usize::MAX),
        });
        let mut arena = Arena::with_budget(budget.clone(), None);
        let mut places = Places::default();
        let source = places.begin(&mut arena, 1, 12, 4, 3).unwrap();
        unsafe {
            source.as_ptr().cast::<i32>().write(42);
        }
        assert!(places.publish(1).is_err());
        for _ in 0..3 {
            places.commit_next(1).unwrap();
        }
        assert!(places.commit_next(1).is_err());
        assert_eq!(places.publish(1).unwrap(), source);
        let baseline = budget.bytes.get();
        budget.limit.set(baseline);
        assert!(places
            .move_value(&mut arena, 2, source.as_ptr(), true)
            .is_err());
        assert_eq!(places.publish(1).unwrap(), source);
        assert_eq!(unsafe { source.as_ptr().cast::<i32>().read() }, 42);
        assert_eq!(budget.bytes.get(), baseline);
        budget.limit.set(usize::MAX);
        // A descriptor reservation succeeds, then the payload allocation fails.
        let registration = std::mem::size_of::<Registration>()
            + std::mem::size_of::<(NonNull<u8>, std::alloc::Layout)>();
        budget.limit.set(baseline + registration);
        assert!(places.begin(&mut arena, 3, 64, 8, 8).is_err());
        assert_eq!(budget.bytes.get(), baseline);
        budget.limit.set(usize::MAX);
        assert_eq!(
            places
                .move_value(&mut arena, 2, source.as_ptr(), true)
                .unwrap(),
            source
        );
        assert!(places.publish(1).is_err());
        unsafe {
            places.end(&mut arena, 2);
        }
        let metadata = budget.bytes.get();
        for _ in 0..1000 {
            places.begin(&mut arena, 2, 0, 8, 0).unwrap();
            assert!(budget.bytes.get() > metadata);
            places.publish(2).unwrap();
            unsafe {
                places.end(&mut arena, 2);
                places.end(&mut arena, 2);
            }
            assert_eq!(budget.bytes.get(), metadata);
        }
        unsafe {
            places.finish(&mut arena);
            places.finish(&mut arena);
        }
        assert_eq!(budget.bytes.get(), 0);
    }

    #[test]
    fn fixed_array_f2a_copy_skips_padding_and_inactive_variants() {
        let nodes = [
            CopyNode {
                offset: 0,
                kind: 3,
                count: 2,
                stride: 0,
                end: 3,
            },
            CopyNode {
                offset: 4,
                kind: 0,
                count: 1,
                stride: 0,
                end: 2,
            },
            CopyNode {
                offset: 8,
                kind: 0,
                count: 8,
                stride: 0,
                end: 3,
            },
        ];
        let mut source = [0xa5u8; 16];
        source[..4].copy_from_slice(&0i32.to_ne_bytes());
        source[4] = 42;
        let mut destination = [0x5au8; 16];
        unsafe {
            copy(destination.as_mut_ptr(), source.as_ptr(), &nodes);
        }
        assert_eq!(&destination[..5], &source[..5]);
        assert_eq!(&destination[5..], &[0x5a; 11]);
        unsafe {
            copy(destination.as_mut_ptr(), destination.as_ptr(), &nodes);
        }
        assert_eq!(destination[4], 42);
    }
}
