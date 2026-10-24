//! Explicit parallel execution with checked code, owning packets and root storage.
mod config;
pub mod standalone;
pub use config::{ExecutionMode, TaskExecutor};
pub(crate) mod pool;
mod storage;
pub use pool::{ParallelExecutor, ParallelSubmitter};
use std::cell::RefCell;
pub use storage::TaskBudget;
pub(crate) use storage::{Lease, LocalShard, Storage};
thread_local! { static CURRENT: RefCell<Option<Storage>> = const { RefCell::new(None) }; }
pub(crate) fn current() -> Option<Storage> {
    CURRENT.with(|root| root.borrow().clone())
}
pub(crate) struct Guard(Option<Storage>);
impl Guard {
    pub(crate) fn enter(root: &Storage) -> Self {
        Self(CURRENT.with(|slot| slot.replace(Some(root.clone()))))
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        CURRENT.with(|slot| slot.replace(self.0.take()));
    }
}

/// Only constructed by the explicit unsafe host registration path.
#[derive(Clone)]
pub(crate) struct Host {
    pub(crate) name: String,
    pub(crate) signature: crate::FunctionSignature,
    pub(crate) entry: super::cps::CpsEntry,
    pub(crate) slots: usize,
    pub(crate) _owner: std::sync::Arc<dyn std::any::Any + Send + Sync>,
}
impl Host {
    pub(crate) fn binding(&self) -> super::bindings::HostBinding {
        super::bindings::HostBinding {
            abi: super::bindings::HostAbiKind::Scalar,
            signature: self.signature.clone(),
            entry: super::bindings::HostEntry::Cps {
                entry: self.entry,
                frame_slots: Some(self.slots),
            },
            owner: Some(std::rc::Rc::new(self.clone())),
        }
    }
    pub(crate) fn from_binding(binding: &super::bindings::HostBinding) -> Option<Self> {
        let host = binding.owner.as_ref()?.downcast_ref::<Self>()?;
        let super::bindings::HostEntry::Cps {
            entry,
            frame_slots: Some(slots),
        } = binding.entry
        else {
            return None;
        };
        (std::ptr::fn_addr_eq(entry, host.entry)
            && slots == host.slots
            && binding.signature == host.signature)
            .then(|| host.clone())
    }
}

pub(crate) fn reserve_current(bytes: usize) -> Result<Option<Lease>, crate::Error> {
    current()
        .map(|root| root.accounts().reserve(bytes))
        .transpose()
}

/// Exclusive ownership packet. It is never shared, and it carries the allocation
/// root through publication, failed publication and terminal cleanup.
pub(crate) struct OwnedBuffer {
    owner: Option<std::ptr::NonNull<super::buffer::Buffer>>,
    root: Storage,
    _charge: Lease,
}
// Constructor validates all control blocks and rejects local callbacks, resources
// and accounting pointers. The caller transfers exclusive language ownership;
// the packet retains stable storage and disposes hook-free owners on any thread.
unsafe impl Send for OwnedBuffer {}
impl OwnedBuffer {
    /// # Safety
    /// `owner` is a live exclusive compiler-owned value. The caller retains its
    /// cleanup responsibility on error and relinquishes it only on success.
    pub(crate) unsafe fn new(
        arena: &super::arena::Arena,
        owner: std::ptr::NonNull<super::buffer::Buffer>,
    ) -> Result<Self, crate::Error> {
        super::buffer::Buffer::validate_parallel_owner(arena, owner.as_ptr())?;
        let root = arena
            .parallel_root()
            .expect("validated parallel root")
            .clone();
        let charge = root.accounts().reserve(std::mem::size_of::<Self>())?;
        Ok(Self {
            owner: Some(owner),
            root,
            _charge: charge,
        })
    }
    pub(crate) fn take(
        &mut self,
        arena: &super::arena::Arena,
    ) -> Result<*mut super::buffer::Buffer, crate::Error> {
        if !arena
            .parallel_root()
            .is_some_and(|root| self.root.same_root(root))
        {
            return Err(crate::Error::new_with_code(
                crate::ErrorPhase::Runtime,
                crate::ErrorCode::InvalidCall,
                "Owner packet belongs to a different root",
            ));
        }
        self.owner
            .take()
            .map(std::ptr::NonNull::as_ptr)
            .ok_or_else(|| {
                crate::Error::new_with_code(
                    crate::ErrorPhase::Runtime,
                    crate::ErrorCode::InvalidState,
                    "Owner packet was already consumed",
                )
            })
    }
}
impl Drop for OwnedBuffer {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.take() {
            let mut arena = super::arena::Arena::with_storage(self.root.clone());
            // Admission excludes hooks; cleanup cannot invoke user code.
            unsafe { super::buffer::Buffer::release(&mut arena, owner.as_ptr()) }
                .expect("admitted owner cleanup");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        arena::Arena,
        buffer::{Buffer, ElementCleanup},
    };
    #[test]
    fn parallel_owning_tree_keeps_addresses_and_metadata_until_root_release() {
        let budget = TaskBudget::new(1 << 20);
        let root = Storage::new(vec![budget.clone()]).unwrap();
        let mut arena = Arena::with_storage(root.clone());
        let text = Buffer::copy_bytes(&mut arena, b"retained payload").unwrap();
        let mut outer = Buffer::allocate(&mut arena, 1, 8, 8).unwrap();
        unsafe {
            outer
                .as_mut()
                .configure(ElementCleanup {
                    owners: vec![0],
                    hooks: vec![],
                })
                .unwrap();
            outer
                .as_ref()
                .initialize_next()
                .unwrap()
                .cast::<*mut Buffer>()
                .write(text.as_ptr());
            outer.as_mut().commit_next();
        }
        let packet = unsafe { OwnedBuffer::new(&arena, outer) }.unwrap();
        let before = budget.used();
        let address = text.as_ptr() as usize;
        drop(arena);
        assert_eq!(
            budget.used(),
            before,
            "ending a branch must not refund retained allocations"
        );
        let receiver_root = root.clone();
        std::thread::spawn(move || {
            let mut packet = packet;
            let foreign =
                Arena::with_storage(Storage::new(vec![TaskBudget::new(1 << 20)]).unwrap());
            assert!(packet.take(&foreign).is_err());
            let mut receiver = Arena::with_storage(receiver_root);
            let outer = packet.take(&receiver).unwrap();
            unsafe {
                let child = (*outer).load(0).unwrap().cast::<*mut Buffer>().read();
                assert_eq!(child as usize, address);
                assert_eq!(
                    crate::runtime::owned_text::borrow(child).unwrap(),
                    "retained payload"
                );
                Buffer::release(&mut receiver, outer).unwrap();
            }
            assert!(packet.take(&receiver).is_err());
        })
        .join()
        .unwrap();
        assert!(
            budget.used() > 0,
            "the root still owns shard registration capacity"
        );
        drop(root);
        assert_eq!(budget.used(), 0);
    }
}
