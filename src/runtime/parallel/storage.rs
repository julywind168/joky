//! Synchronized accounting and nonmoving, root-retained allocation shards.
use crate::{Error, ErrorCode, ErrorPhase};
use std::{
    alloc::{alloc, dealloc, Layout},
    ptr::NonNull,
    sync::{Arc, Mutex, MutexGuard},
};

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn limited() -> Error {
    Error::new_with_code(
        ErrorPhase::Runtime,
        ErrorCode::ResourceLimit,
        "Parallel task storage budget exceeded",
    )
}

/// A shared live-byte ceiling. Reservations across multiple ceilings are atomic.
#[derive(Clone)]
pub struct TaskBudget(Arc<Budget>);
struct Budget {
    limit: usize,
    used: Mutex<usize>,
}
impl TaskBudget {
    pub fn new(limit: usize) -> Self {
        Self(Arc::new(Budget {
            limit,
            used: Mutex::new(0),
        }))
    }
    pub fn used(&self) -> usize {
        *lock(&self.0.used)
    }
}

#[derive(Clone)]
pub(crate) struct Accounts(Arc<[TaskBudget]>);
impl Accounts {
    pub(crate) fn empty_lease(&self) -> Lease {
        Lease {
            accounts: self.clone(),
            bytes: 0,
        }
    }

    pub(crate) fn new(mut budgets: Vec<TaskBudget>) -> Self {
        budgets.sort_by_key(|budget| Arc::as_ptr(&budget.0));
        budgets.dedup_by(|a, b| Arc::ptr_eq(&a.0, &b.0));
        Self(budgets.into())
    }
    pub(crate) fn reserve(&self, amount: usize) -> Result<Lease, Error> {
        let mut lease = Lease {
            accounts: self.clone(),
            bytes: 0,
        };
        lease.resize(amount)?;
        Ok(lease)
    }
}
/// Unique refund authority. Moving a packet never refunds its storage.
pub(crate) struct Lease {
    accounts: Accounts,
    bytes: usize,
}
impl Lease {
    pub(crate) fn add(&mut self, amount: usize) -> Result<(), Error> {
        self.resize(self.bytes.checked_add(amount).ok_or_else(limited)?)
    }
    pub(crate) fn refund(&mut self, amount: usize) {
        assert!(self.bytes >= amount);
        for budget in self.accounts.0.iter() {
            *lock(&budget.0.used) -= amount;
        }
        self.bytes -= amount;
    }

    pub(crate) fn resize(&mut self, bytes: usize) -> Result<(), Error> {
        // The sorted identities impose one lock order even for overlapping roots.
        let mut guards: Vec<_> = self.accounts.0.iter().map(|b| lock(&b.0.used)).collect();
        if bytes > self.bytes {
            let additional = bytes - self.bytes;
            if guards
                .iter()
                .zip(self.accounts.0.iter())
                .any(|(used, budget)| {
                    used.checked_add(additional)
                        .is_none_or(|n| n > budget.0.limit)
                })
            {
                return Err(limited());
            }
            for used in &mut guards {
                **used += additional;
            }
        } else {
            for used in &mut guards {
                **used -= self.bytes - bytes;
            }
        }
        self.bytes = bytes;
        Ok(())
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        // Refund needs no allocation, including during unwinding or OOM cleanup.
        for budget in self.accounts.0.iter() {
            *lock(&budget.0.used) -= self.bytes;
        }
    }
}

struct Allocation {
    task: Option<u64>,
    payload_size: usize,
    finalize: Option<unsafe fn(*mut u8)>,
    pointer: NonNull<u8>,
    layout: Layout,
    _charge: Lease,
}
// This wrapper owns raw storage and explicitly audited thread-safe metadata
// finalizers, never language destructors or local callbacks. Access to payloads
// is governed by checked language ownership and packet admission.
// The shard mutex serializes registration/removal. A removed allocation has no
// outstanding language capability; root destruction occurs after the last reader.
unsafe impl Send for Allocation {}
impl Drop for Allocation {
    fn drop(&mut self) {
        unsafe {
            if let Some(finalize) = self.finalize {
                finalize(self.pointer.as_ptr());
            }
            dealloc(self.pointer.as_ptr(), self.layout);
        }
    }
}
struct Shard {
    allocations: Vec<Allocation>,
    metadata: Lease,
    _charge: Lease,
}
#[derive(Clone)]
pub(crate) struct Storage(Arc<Root>);
struct Root {
    _charge: Lease,
    accounts: Accounts,
    shards: Mutex<Vec<Arc<Mutex<Shard>>>>,
}
pub(crate) struct LocalShard {
    root: Storage,
    shard: Arc<Mutex<Shard>>,
}
impl Storage {
    pub(crate) fn new(budgets: Vec<TaskBudget>) -> Result<Self, Error> {
        let accounts = Accounts::new(budgets);
        let charge = accounts.reserve(
            std::mem::size_of::<Root>() + accounts.0.len() * std::mem::size_of::<TaskBudget>(),
        )?;
        Ok(Self(Arc::new(Root {
            _charge: charge,
            accounts,
            shards: Mutex::new(Vec::new()),
        })))
    }
    pub(crate) fn same_root(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub(crate) fn accounts(&self) -> &Accounts {
        &self.0.accounts
    }
    pub(crate) fn shard(&self) -> Result<LocalShard, Error> {
        let mut shards = lock(&self.0.shards);
        // Completed Local commands must not leave one charged shard per call.
        // A detached packet still has allocations, and a live arena retains an Arc.
        if let Some(shard) = shards
            .iter()
            .find(|s| Arc::strong_count(s) == 1 && lock(s).allocations.is_empty())
        {
            return Ok(LocalShard {
                root: self.clone(),
                shard: shard.clone(),
            });
        }
        let charge = self
            .accounts()
            .reserve(std::mem::size_of::<Shard>() + 2 * std::mem::size_of::<Arc<Mutex<Shard>>>())?;
        shards.try_reserve_exact(1).map_err(|_| limited())?;
        let shard = Arc::new(Mutex::new(Shard {
            allocations: Vec::new(),
            metadata: self.accounts().reserve(0)?,
            _charge: charge,
        }));
        shards.push(shard.clone());
        Ok(LocalShard {
            root: self.clone(),
            shard,
        })
    }
    pub(crate) fn contains(&self, pointer: NonNull<u8>) -> bool {
        lock(&self.0.shards)
            .iter()
            .any(|shard| lock(shard).allocations.iter().any(|a| a.pointer == pointer))
    }
    /// Compiler-checked references establish initialization and type. Allocation
    /// identity additionally proves this is live storage owned by this task,
    /// rather than another task's allocation in the same shared storage root.
    pub(crate) fn owns_range(&self, data: NonNull<u8>, size: usize, align: usize) -> bool {
        let Some(task) = super::pool::identity() else {
            return false;
        };
        if !(data.as_ptr() as usize).is_multiple_of(align) {
            return false;
        }
        lock(&self.0.shards).iter().any(|shard| {
            lock(shard).allocations.iter().any(|allocation| {
                allocation.task == Some(task)
                    && (data.as_ptr() as usize)
                        .checked_sub(allocation.pointer.as_ptr() as usize)
                        .and_then(|offset| offset.checked_add(size))
                        .is_some_and(|end| end <= allocation.payload_size)
            })
        })
    }

    /// Exclusive owned-packet adoption changes the task allowed to originate
    /// ordinary borrows from this allocation. ZST sentinels have no allocation.
    ///
    /// # Safety
    /// The caller is installing the exclusive payload owner in the current task;
    /// no outstanding language borrow or previous owner may still access it.
    pub(crate) unsafe fn adopt_allocation(&self, pointer: NonNull<u8>) {
        for shard in lock(&self.0.shards).iter() {
            if let Some(allocation) = lock(shard)
                .allocations
                .iter_mut()
                .find(|a| a.pointer == pointer)
            {
                allocation.task = super::pool::identity();
                return;
            }
        }
    }

    /// Install only a thread-safe runtime metadata finalizer after initialization.
    /// No language destructor or local callback may be installed here.
    pub(crate) fn finalize_with(&self, pointer: NonNull<u8>, finalize: unsafe fn(*mut u8)) {
        for shard in lock(&self.0.shards).iter() {
            let mut shard = lock(shard);
            if let Some(allocation) = shard.allocations.iter_mut().find(|a| a.pointer == pointer) {
                assert!(allocation.finalize.is_none());
                allocation.finalize = Some(finalize);
                return;
            }
        }
        panic!("validated metadata allocation");
    }
    /// # Safety
    /// No capability may access `pointer` after this call, on any worker.
    pub(crate) unsafe fn release(&self, pointer: NonNull<u8>) {
        let shards = lock(&self.0.shards);
        for shard in shards.iter() {
            let mut shard = lock(shard);
            if let Some(index) = shard.allocations.iter().position(|a| a.pointer == pointer) {
                drop(shard.allocations.swap_remove(index));
                return;
            }
        }
        panic!("validated parallel allocation");
    }
}
impl LocalShard {
    /// Reclaim a private region before the enclosing task tree closes.
    ///
    /// # Safety
    /// No references or owners into this shard may remain live or be exported.
    /// The caller exclusively owns the region and runs on its owning worker.
    pub(crate) unsafe fn release_region(&mut self) {
        let allocations = {
            let mut shard = lock(&self.shard);
            let allocations = std::mem::take(&mut shard.allocations);
            shard.metadata.resize(0).expect("refund region metadata");
            allocations
        };
        // Keep even runtime finalizers outside the shard lock.
        drop(allocations);
    }
    pub(crate) fn bytes(&self) -> usize {
        lock(&self.shard)
            .allocations
            .iter()
            .map(|a| a._charge.bytes)
            .sum()
    }
    pub(crate) fn allocate(&mut self, size: usize, align: usize) -> Result<NonNull<u8>, Error> {
        let layout = Layout::from_size_align(size.max(1), align).map_err(|_| limited())?;
        let bytes = layout.pad_to_align().size();
        let charge = self.root.accounts().reserve(bytes)?;
        let mut shard = lock(&self.shard);
        if shard.allocations.len() == shard.allocations.capacity() {
            let capacity = shard.allocations.len().checked_add(1).ok_or_else(limited)?;
            let old = shard.metadata.bytes;
            shard.metadata.resize(
                capacity
                    .checked_mul(std::mem::size_of::<Allocation>())
                    .ok_or_else(limited)?,
            )?;
            if shard.allocations.try_reserve_exact(1).is_err() {
                shard.metadata.resize(old).expect("refund metadata");
                return Err(limited());
            }
        }
        let pointer = NonNull::new(unsafe { alloc(layout) }).ok_or_else(limited)?;
        shard.allocations.push(Allocation {
            task: super::pool::identity(),
            payload_size: size,
            finalize: None,
            pointer,
            layout,
            _charge: charge,
        });
        Ok(pointer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    #[test]
    fn parallel_storage_reuses_only_empty_unowned_shards() {
        let budget = TaskBudget::new(1 << 20);
        let root = Storage::new(vec![budget.clone()]).unwrap();
        let mut first = root.shard().unwrap();
        let detached = first.allocate(8, 8).unwrap();
        drop(first);
        let second = root.shard().unwrap();
        assert_eq!(
            lock(&root.0.shards).len(),
            2,
            "detached owners retain their shard"
        );
        unsafe {
            root.release(detached);
        }
        let third = root.shard().unwrap();
        assert!(
            !Arc::ptr_eq(&second.shard, &third.shard),
            "live empty arenas remain exclusive"
        );
        drop((second, third));
        let mut shard = root.shard().unwrap();
        let value = shard.allocate(8, 8).unwrap();
        unsafe {
            root.release(value);
        }
        drop(shard);
        let stable = budget.used();
        for _ in 0..128 {
            let mut shard = root.shard().unwrap();
            let value = shard.allocate(8, 8).unwrap();
            unsafe {
                root.release(value);
            }
            drop(shard);
            assert_eq!(budget.used(), stable);
        }
        drop(root);
        assert_eq!(budget.used(), 0);
    }
    #[test]
    fn parallel_shared_budgets_reserve_all_or_nothing() {
        let one = TaskBudget::new(100);
        let two = TaskBudget::new(100);
        let gate = Arc::new(Barrier::new(3));
        let release = Arc::new(Barrier::new(3));
        let workers: Vec<_> = [
            vec![one.clone(), two.clone()],
            vec![two.clone(), one.clone(), one.clone()],
        ]
        .into_iter()
        .map(|budgets| {
            let gate = gate.clone();
            let release = release.clone();
            std::thread::spawn(move || {
                let accounts = Accounts::new(budgets);
                gate.wait();
                let lease = accounts.reserve(80);
                release.wait();
                let ok = lease.is_ok();
                gate.wait();
                drop(lease);
                ok
            })
        })
        .collect();
        gate.wait();
        release.wait();
        assert_eq!(one.used(), 80);
        assert_eq!(two.used(), 80);
        gate.wait();
        let wins = workers
            .into_iter()
            .map(|worker| usize::from(worker.join().unwrap()))
            .sum::<usize>();
        assert_eq!(wins, 1);
        assert_eq!(one.used(), 0);
        assert_eq!(two.used(), 0);
    }
    #[test]
    fn parallel_unregistered_buffer_refunds_metadata_and_drops_budget_references() {
        let budget = TaskBudget::new(1 << 20);
        let weak = Arc::downgrade(&budget.0);
        let root = Storage::new(vec![budget.clone()]).unwrap();
        let mut arena = crate::runtime::arena::Arena::with_storage(root.clone());
        // Simulate failure between initialized allocation and cleanup registration.
        crate::runtime::buffer::Buffer::copy_bytes(&mut arena, b"unpublished").unwrap();
        drop(arena);
        assert!(budget.used() > 0);
        drop(root);
        assert_eq!(budget.used(), 0);
        drop(budget);
        assert!(weak.upgrade().is_none());
    }
}
