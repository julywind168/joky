//! Frozen owning graphs. Only each handle's control block belongs to a worker;
//! the graph has no mutable views, resource callbacks, or arena accounting pointers.
use super::{bounds_error, Buffer};
use crate::{
    runtime::{
        arena::{Arena, ExternalCharge},
        parallel::{Lease, WeakStorage},
        resource::{OwnedResource, ResourceReleaseV1},
        tcp::OwnerCharge,
    },
    Error,
};
use std::{alloc::Layout, collections::HashMap, ffi::c_void, ptr::NonNull, rc::Rc, sync::Arc};

struct Payload {
    pointer: NonNull<u8>,
    layout: Layout,
}
impl Drop for Payload {
    fn drop(&mut self) {
        unsafe { std::alloc::dealloc(self.pointer.as_ptr(), self.layout) };
    }
}
struct Node {
    header: Box<Buffer>,
    _payload: Payload,
}
struct Frozen {
    nodes: Vec<Node>,
    root: Option<WeakStorage>,
    _charge: Option<Lease>,
}
// SAFETY: the checked type contains only reference-free data, immutable text,
// containers and noncapturing language function values. Freeze replaces every
// owning pointer with a private immutable header; those headers have no resource,
// cleanup descriptor, loan, dynamic table or local accounting pointer. No mutable
// reference to this graph is exposed. Handles keep all nodes alive while read.
unsafe impl Send for Frozen {}
unsafe impl Sync for Frozen {}

struct Owner {
    core: Arc<Frozen>,
    // Local counters never enter a transferable core or a remote worker.
    local_charge: Option<Rc<ExternalCharge>>,
    _charge: OwnerCharge,
}
unsafe extern "C" fn release(token: *mut c_void) {
    drop(Box::from_raw(token.cast::<Owner>()));
}

pub(super) fn is_handle(resource: &OwnedResource) -> bool {
    resource.control_token(release).is_ok()
}

/// # Safety
/// The resource ABI owns `token`; the release identity is checked before its layout.
pub(crate) unsafe fn transferable(token: NonNull<c_void>, callback: ResourceReleaseV1) -> bool {
    if !std::ptr::fn_addr_eq(callback, release as ResourceReleaseV1) {
        return false;
    }
    let owner = token.cast::<Owner>().as_ref();
    owner.core.root.is_some() && owner.local_charge.is_none() && owner._charge.shared()
}

fn adopt(
    arena: &mut Arena,
    core: Arc<Frozen>,
    local_charge: Option<Rc<ExternalCharge>>,
) -> Result<NonNull<Buffer>, Error> {
    let charge = OwnerCharge::new(
        arena,
        std::mem::size_of::<Owner>() + OwnedResource::event_bytes(),
    )?;
    let root = &core.nodes[0].header;
    let (data, len, element) = (root.data, root.len, root.element);
    let token = NonNull::from(Box::leak(Box::new(Owner {
        core,
        local_charge,
        _charge: charge,
    })));
    let resource = OwnedResource::from_token(token.cast(), release)
        .inspect_err(|_| unsafe { release(token.as_ptr().cast()) })?;
    let mut handle = Buffer::external(arena, resource)?;
    // The resource owns the graph. Capacity and cleanup stay empty so ordinary
    // Buffer release never treats shared bytes as a task-owned allocation.
    unsafe {
        let handle = handle.as_mut();
        handle.data = data;
        handle.len = len;
        handle.element = element;
    }
    Ok(handle)
}

/// # Safety
/// `source` is a live compiler-checked Buffer<T> in `arena`. Freezing accepts
/// exactly one initialized, reference-free element and does not consume it.
/// Sharing accepts only a handle produced here. The caller keeps its input alive
/// throughout this synchronous call and exposes the result only through Shared<T>.
pub(crate) unsafe fn create(
    arena: &mut Arena,
    source: *mut Buffer,
    freeze: bool,
) -> Result<NonNull<Buffer>, Error> {
    Buffer::validate_owner(arena, source)?;
    if !freeze {
        let owner = &*(*source)
            .resource_ref()?
            .control_token(release)?
            .cast::<Owner>();
        let same_root = match (&owner.core.root, arena.parallel_root(), &owner.local_charge) {
            (Some(root), Some(current), None) => root.same_root(current),
            (None, None, Some(charge)) => charge.belongs_to(arena),
            _ => false,
        };
        if !same_root {
            return Err(bounds_error("Shared handles cannot cross task roots"));
        }
        return adopt(arena, owner.core.clone(), owner.local_charge.clone());
    }
    if (*source).len != 1 {
        return Err(bounds_error(
            "Shared freezing requires one initialized value",
        ));
    }
    let graph = Buffer::owning_allocations(arena, [source])?;
    let overflow = || {
        Error::new_with_code(
            crate::ErrorPhase::Runtime,
            crate::ErrorCode::ResourceLimit,
            "Shared graph exceeds its allocation capacity",
        )
    };
    let count = graph.buffers.len();
    let scratch_bytes = graph
        .allocations
        .capacity()
        .checked_mul(std::mem::size_of::<NonNull<u8>>())
        .and_then(|bytes| {
            bytes.checked_add(
                graph
                    .buffers
                    .capacity()
                    .checked_mul(std::mem::size_of::<NonNull<Buffer>>())?,
            )
        })
        // HashMap rounds bucket counts upward and reserves control bytes. Include
        // that slack before allocating the temporary address translation table.
        .and_then(|bytes| bytes.checked_add(count.checked_mul(8 * std::mem::size_of::<usize>())?))
        .and_then(|bytes| bytes.checked_add(8 * std::mem::size_of::<usize>()))
        .ok_or_else(overflow)?;
    let _scratch = arena.reserve_external(scratch_bytes)?;
    let mut bytes = std::mem::size_of::<Frozen>() + 2 * std::mem::size_of::<usize>();
    for pointer in &graph.buffers {
        crate::runtime::parallel::pool::check_cancelled()?;
        let buffer = pointer.as_ref();
        if buffer.resource.is_some()
            || buffer.dynamic.is_some()
            || buffer.failed_store.is_some()
            || buffer.failed_descriptor
            || buffer.release_active
        {
            return Err(bounds_error("Shared values require plain immutable data"));
        }
        let payload = buffer
            .len
            .checked_mul(buffer.element.size())
            .ok_or_else(overflow)?
            .max(1);
        bytes = bytes
            .checked_add(payload)
            .and_then(|bytes| {
                bytes.checked_add(std::mem::size_of::<Node>() + std::mem::size_of::<Buffer>())
            })
            .ok_or_else(overflow)?;
    }
    let (root, charge, local_charge) = if let Some(root) = arena.parallel_root() {
        (
            Some(root.downgrade()),
            Some(root.accounts().reserve(bytes)?),
            None,
        )
    } else {
        let bytes = bytes
            .checked_add(std::mem::size_of::<ExternalCharge>() + 2 * std::mem::size_of::<usize>())
            .ok_or_else(overflow)?;
        (None, None, Some(Rc::new(arena.reserve_external(bytes)?)))
    };
    let mut frozen = Frozen {
        nodes: Vec::new(),
        root,
        _charge: charge,
    };
    frozen
        .nodes
        .try_reserve_exact(count)
        .map_err(|_| overflow())?;
    let mut addresses = HashMap::new();
    addresses.try_reserve(count).map_err(|_| overflow())?;
    for pointer in &graph.buffers {
        crate::runtime::parallel::pool::check_cancelled()?;
        let source = pointer.as_ref();
        let size = (source.len * source.element.size()).max(1);
        let layout =
            Layout::from_size_align(size, source.element.align()).map_err(|_| overflow())?;
        let data = NonNull::new(std::alloc::alloc(layout)).ok_or_else(overflow)?;
        let payload = Payload {
            pointer: data,
            layout,
        };
        let length = source.len * source.element.size();
        for start in (0..length).step_by(super::BYTE_CHUNK) {
            crate::runtime::parallel::pool::check_cancelled()?;
            std::ptr::copy_nonoverlapping(
                source.data.as_ptr().add(start),
                data.as_ptr().add(start),
                (length - start).min(super::BYTE_CHUNK),
            );
        }
        let mut header = Box::new(Buffer::empty(source.element));
        header.data = data;
        header.len = source.len;
        // Only the initialized prefix was allocated in the frozen snapshot.
        header.capacity = source.len;
        addresses.insert(pointer.as_ptr(), NonNull::from(header.as_mut()).as_ptr());
        frozen.nodes.push(Node {
            header,
            _payload: payload,
        });
    }
    // Discovery validated a unique owning tree. Rewrite every active owner lane;
    // inactive enum lanes remain null. No cleanup ever follows these pointers.
    for (original, copy) in graph.buffers.iter().zip(&frozen.nodes) {
        crate::runtime::parallel::pool::check_cancelled()?;
        let original = original.as_ref();
        if let Some(cleanup) = &original.cleanup {
            for index in 0..original.len {
                if index.is_multiple_of(256) {
                    crate::runtime::parallel::pool::check_cancelled()?;
                }
                for &offset in &cleanup.owners {
                    if offset == usize::MAX {
                        continue;
                    }
                    let child = original
                        .address(index)
                        .add(offset)
                        .cast::<*mut Buffer>()
                        .read_unaligned();
                    let translated = if child.is_null() {
                        child
                    } else {
                        addresses[&child]
                    };
                    copy.header
                        .address(index)
                        .add(offset)
                        .cast::<*mut Buffer>()
                        .write_unaligned(translated);
                }
            }
        }
    }
    adopt(arena, Arc::new(frozen), local_charge)
}

/// # Safety
/// The pointers satisfy `create`; source locations are compiler-provided.
#[export_name = "__joky_buffer_shared_freeze"]
pub(crate) unsafe extern "C" fn freeze_native(
    arena: *mut Arena,
    source: *mut Buffer,
    file: u64,
    start: usize,
    end: usize,
) -> *mut Buffer {
    super::located(
        create(&mut *arena, source, true).map(NonNull::as_ptr),
        file,
        start,
        end,
    )
}

/// # Safety
/// The pointers satisfy `create`; source locations are compiler-provided.
#[export_name = "__joky_buffer_shared_clone"]
pub(crate) unsafe extern "C" fn clone_native(
    arena: *mut Arena,
    source: *mut Buffer,
    file: u64,
    start: usize,
    end: usize,
) -> *mut Buffer {
    super::located(
        create(&mut *arena, source, false).map(NonNull::as_ptr),
        file,
        start,
        end,
    )
}

#[cfg(test)]
mod tests;
