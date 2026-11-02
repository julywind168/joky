//! Explicit parallel execution with checked code, owning packets and root storage.
mod config;
pub(crate) mod control;
pub mod standalone;
pub use config::{ExecutionMode, TaskExecutor};
pub(crate) mod packet;
pub(crate) mod place;
pub(crate) mod pool;
mod portable;
pub(crate) mod scoped;
mod storage;
pub use pool::{ParallelExecutor, ParallelSubmitter};
use std::cell::RefCell;
pub use storage::TaskBudget;
pub(crate) use storage::{Lease, LocalShard, SharedLease, Storage, WeakStorage};
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

/// Constructed by explicit unsafe host registration or audited built-in adapters.
/// Frozen evidence owns code dependencies without carrying a local registry.
#[derive(Clone)]
pub(crate) struct Host {
    pub(crate) name: String,
    pub(crate) signature: crate::FunctionSignature,
    pub(crate) code: HostCode,
    pub(crate) _owner: std::sync::Arc<dyn std::any::Any + Send + Sync>,
}
/// Each variant fixes an ABI, rather than blessing arbitrary direct pointers.
#[derive(Clone, Copy)]
pub(crate) enum HostCode {
    Cps {
        entry: super::cps::CpsEntry,
        slots: usize,
    },
    /// Synchronous immutable TextViewV2 input, no retained pointer or callback.
    /// Only audited runtime bindings construct this internal variant.
    TextView(unsafe extern "C" fn(*const u8) -> i32),
    Time(crate::stdlib::time::Operation),
    Tcp(super::tcp::Operation),
    Dns,
    Password,
    Postgres(super::postgres::Operation),
    Tls(super::tls::Operation),
    Shutdown(super::shutdown::Operation),
    Control(control::Operation),
}
impl Host {
    pub(crate) fn binding(&self) -> super::bindings::HostBinding {
        use super::bindings::{HostAbiKind, HostEntry};
        let (abi, entry) = match self.code {
            HostCode::Time(op) => (HostAbiKind::Scalar, HostEntry::Direct(op.entry())),
            HostCode::Control(op) => (
                HostAbiKind::Scalar,
                match op {
                    control::Operation::VmCreate => {
                        HostEntry::ResourceFactory(std::rc::Rc::new(crate::stdlib::VM_FACTORY))
                    }
                    control::Operation::VmClose => {
                        HostEntry::Direct(crate::stdlib::VM_CLOSE as *const u8)
                    }
                    control::Operation::VmCall => HostEntry::VmCall,
                    control::Operation::VmSource => HostEntry::VmSource,
                    control::Operation::VmMigration(op) => HostEntry::VmMigration(op),
                    control::Operation::Domain(op) => HostEntry::Domain(op),
                    control::Operation::Fs(op) => HostEntry::Fs(op),
                },
            ),
            HostCode::Postgres(op) => (HostAbiKind::Scalar, HostEntry::Postgres(op)),
            HostCode::Tls(op) => (HostAbiKind::Scalar, HostEntry::Tls(op)),
            HostCode::Shutdown(op) => (HostAbiKind::Scalar, HostEntry::Shutdown(op)),
            HostCode::Password => (HostAbiKind::Scalar, HostEntry::Password),
            HostCode::Dns => (HostAbiKind::Scalar, HostEntry::Dns),
            HostCode::Tcp(op) => (HostAbiKind::Scalar, HostEntry::Tcp(op)),
            HostCode::Cps { entry, slots } => (
                HostAbiKind::Scalar,
                HostEntry::Cps {
                    entry,
                    frame_slots: Some(slots),
                },
            ),
            HostCode::TextView(entry) => (
                HostAbiKind::TextViewV2,
                HostEntry::Direct(entry as *const u8),
            ),
        };
        super::bindings::HostBinding {
            abi,
            signature: self.signature.clone(),
            entry,
            owner: Some(std::rc::Rc::new(self.clone())),
        }
    }
    pub(crate) fn from_binding(binding: &super::bindings::HostBinding) -> Option<Self> {
        use super::bindings::{HostAbiKind, HostEntry};
        if let HostEntry::Postgres(op) = binding.entry {
            if binding.abi != HostAbiKind::Scalar || op.validate(&binding.signature, true).is_err()
            {
                return None;
            }
            return Some(Self {
                name: format!("{op:?}"),
                signature: binding.signature.clone(),
                code: HostCode::Postgres(op),
                _owner: std::sync::Arc::new(()),
            });
        }
        if let HostEntry::Tls(op) = binding.entry {
            if binding.abi != HostAbiKind::Scalar || op.validate(&binding.signature, true).is_err()
            {
                return None;
            }
            return Some(Self {
                name: format!("{op:?}"),
                signature: binding.signature.clone(),
                code: HostCode::Tls(op),
                _owner: std::sync::Arc::new(()),
            });
        }
        if let HostEntry::Shutdown(op) = binding.entry {
            if binding.abi != HostAbiKind::Scalar || op.validate(&binding.signature, true).is_err()
            {
                return None;
            }
            return Some(Self {
                name: format!("{op:?}"),
                signature: binding.signature.clone(),
                code: HostCode::Shutdown(op),
                _owner: std::sync::Arc::new(()),
            });
        }
        if matches!(binding.entry, HostEntry::Password) {
            if binding.abi != HostAbiKind::Scalar
                || super::password::validate(&binding.signature, true).is_err()
            {
                return None;
            }
            return Some(Self {
                name: "__password_compute".into(),
                signature: binding.signature.clone(),
                code: HostCode::Password,
                _owner: std::sync::Arc::new(()),
            });
        }
        if matches!(binding.entry, HostEntry::Dns) {
            if binding.abi != HostAbiKind::Scalar
                || super::dns::validate(&binding.signature, true).is_err()
            {
                return None;
            }
            return Some(Self {
                name: "__dns_lookup".into(),
                signature: binding.signature.clone(),
                code: HostCode::Dns,
                _owner: std::sync::Arc::new(()),
            });
        }
        if let HostEntry::Tcp(op) = binding.entry {
            if binding.abi != HostAbiKind::Scalar || op.validate(&binding.signature, true).is_err()
            {
                return None;
            }
            return Some(Self {
                name: format!("{op:?}"),
                signature: binding.signature.clone(),
                code: HostCode::Tcp(op),
                _owner: std::sync::Arc::new(()),
            });
        }
        if matches!(binding.entry, HostEntry::Cps { entry, frame_slots: Some(1) } if std::ptr::fn_addr_eq(entry,crate::stdlib::VM_SHARE as super::cps::CpsEntry))
            && binding.signature == crate::core::vm::binding("__vm_share")?.signature
            && binding.abi == HostAbiKind::Scalar
        {
            return Some(Self {
                name: "__vm_share".into(),
                signature: binding.signature.clone(),
                code: HostCode::Cps {
                    entry: crate::stdlib::VM_SHARE,
                    slots: 1,
                },
                _owner: std::sync::Arc::new(()),
            });
        }
        let control = match &binding.entry {
            HostEntry::Direct(entry)
                if *entry == crate::stdlib::VM_CLOSE as *const u8
                    && binding.signature == crate::core::vm::binding("__vm_close")?.signature =>
            {
                Some(control::Operation::VmClose)
            }
            HostEntry::VmCall if binding.signature == crate::core::vm::call_signature() => {
                Some(control::Operation::VmCall)
            }
            HostEntry::VmMigration(op) if binding.signature == op.signature() => {
                Some(control::Operation::VmMigration(*op))
            }
            HostEntry::VmSource if binding.signature == crate::core::vm::source_signature() => {
                Some(control::Operation::VmSource)
            }
            HostEntry::Domain(op) if op.validate(&binding.signature, true).is_ok() => {
                control::Operation::domain(*op)
            }
            HostEntry::Fs(op) if op.validate(&binding.signature, true).is_ok() => {
                Some(control::Operation::Fs(*op))
            }
            HostEntry::ResourceFactory(f)
                if control::vm_factory(f)
                    && binding.signature
                        == crate::FunctionSignature::new(
                            vec![crate::Type::UInt64],
                            crate::Type::Resource("vm.Owner".into()),
                        ) =>
            {
                Some(control::Operation::VmCreate)
            }
            _ => None,
        };
        if let Some(op) = control.filter(|_| binding.abi == HostAbiKind::Scalar) {
            return Some(Self {
                name: format!("{op:?}"),
                signature: binding.signature.clone(),
                code: HostCode::Control(op),
                _owner: std::sync::Arc::new(()),
            });
        }
        let host = binding.owner.as_ref()?.downcast_ref::<Self>()?;
        let matches = match (host.code, &binding.entry) {
            (
                HostCode::Cps {
                    entry: expected,
                    slots: expected_slots,
                },
                HostEntry::Cps {
                    entry,
                    frame_slots: Some(slots),
                },
            ) => {
                binding.abi == HostAbiKind::Scalar
                    && std::ptr::fn_addr_eq(*entry, expected)
                    && *slots == expected_slots
            }
            (HostCode::Time(op), HostEntry::Direct(entry)) => {
                binding.abi == HostAbiKind::Scalar
                    && *entry == op.entry()
                    && binding.signature == op.signature()
            }
            (HostCode::TextView(expected), HostEntry::Direct(entry)) => {
                binding.abi == HostAbiKind::TextViewV2
                    && *entry == expected as *const u8
                    && binding.signature == crate::core::builtins::stdout_signature()
                    && (!matches!(
                        host.name.as_str(),
                        crate::core::builtins::STDOUT_NAME | crate::core::builtins::STDERR_NAME
                    ) || host.is_output())
            }
            _ => false,
        };
        (matches && binding.signature == host.signature).then(|| host.clone())
    }

    pub(crate) fn is_output(&self) -> bool {
        let entry = match self.name.as_str() {
            crate::core::builtins::STDOUT_NAME => crate::stdlib::output::joky_io_write_stdout_v2,
            crate::core::builtins::STDERR_NAME => crate::stdlib::output::joky_io_write_stderr_v2,
            _ => return false,
        };
        self.signature == crate::core::builtins::stdout_signature()
            && matches!(self.code, HostCode::TextView(actual) if std::ptr::fn_addr_eq(
                actual, entry as unsafe extern "C" fn(*const u8) -> i32,
            ))
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
// Constructor validates all control blocks and rejects local callbacks, unqualified
// resources and accounting pointers. The caller transfers exclusive language ownership;
// the packet retains stable storage and disposes owners on any thread. Audited TCP
// resources carry shared refund leases and have no active operation or local Rc.
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
            .map(|owner| {
                unsafe {
                    super::buffer::Buffer::adopt_task(owner.as_ptr());
                }
                owner.as_ptr()
            })
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
    fn parallel_buffer_replacement_moves_metadata_lease_once() {
        let budget = TaskBudget::new(1 << 20);
        let root = Storage::new(vec![budget.clone()]).unwrap();
        let mut arena = Arena::with_storage(root.clone());
        let owner = Buffer::copy_bytes(&mut arena, b"old").unwrap();
        for _ in 0..8 {
            let mut next = Buffer::copy_bytes(&mut arena, b"replacement").unwrap();
            unsafe {
                next.as_mut().reserve_metadata(128).unwrap();
                Buffer::replace(&mut arena, owner.as_ptr(), next.as_ptr()).unwrap();
                assert_eq!(
                    crate::runtime::owned_text::borrow(owner.as_ptr()).unwrap(),
                    "replacement"
                );
            }
        }
        // The transferred lease and header remain valid after the source header
        // is freed, including when the receiver is a different host thread.
        let packet = unsafe { OwnedBuffer::new(&arena, owner) }.unwrap();
        drop((arena, root));
        std::thread::spawn(move || drop(packet)).join().unwrap();
        assert_eq!(budget.used(), 0);
    }

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
