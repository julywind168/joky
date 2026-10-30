//! Synchronized accounting and nonmoving, root-retained allocation shards.
use crate::{Error, ErrorCode, ErrorPhase};
use std::{
    alloc::{alloc, dealloc, Layout},
    ptr::NonNull,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, MutexGuard,
    },
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
    admission: Mutex<()>,
    used: AtomicUsize,
}
impl TaskBudget {
    pub fn new(limit: usize) -> Self {
        Self(Arc::new(Budget {
            limit,
            admission: Mutex::new(()),
            used: AtomicUsize::new(0),
        }))
    }
    pub fn used(&self) -> usize {
        // Do not observe a charge before its multi-budget admission completes.
        let _guard = lock(&self.0.admission);
        self.0.used.load(Ordering::Relaxed)
    }
}

#[derive(Clone)]
pub(crate) struct Accounts(Arc<[TaskBudget]>);
impl Accounts {
    pub(crate) fn shared_lease(&self) -> SharedLease {
        SharedLease {
            accounts: self.clone(),
            bytes: AtomicUsize::new(0),
        }
    }

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
    fn refund(&self, amount: usize) {
        if amount == 0 {
            return;
        }
        for budget in self.0.iter() {
            budget.0.used.fetch_sub(amount, Ordering::Relaxed);
        }
    }
    fn charge(&self, additional: usize) -> Result<(), Error> {
        if additional == 0 {
            return Ok(());
        }
        // The sorted identities impose one lock order even for overlapping roots.
        let _guards: Vec<_> = self.0.iter().map(|b| lock(&b.0.admission)).collect();
        if self.0.iter().any(|budget| {
            budget
                .0
                .used
                .load(Ordering::Relaxed)
                .checked_add(additional)
                .is_none_or(|n| n > budget.0.limit)
        }) {
            return Err(limited());
        }
        // While admission is locked, only unique leases can refund. Usage can
        // therefore only decrease after preflight. RMW updates preserve those
        // concurrent refunds; a load/store pair would lose them. These counters
        // account for bytes, not publication of the storage they cover.
        for budget in self.0.iter() {
            budget.0.used.fetch_add(additional, Ordering::Relaxed);
        }
        Ok(())
    }
}
/// Unique refund authority. Moving a packet never refunds its storage.
pub(crate) struct Lease {
    accounts: Accounts,
    bytes: usize,
}
impl Lease {
    pub(crate) fn refund(&mut self, amount: usize) {
        assert!(self.bytes >= amount);
        self.accounts.refund(amount);
        self.bytes -= amount;
    }
    pub(crate) fn resize(&mut self, bytes: usize) -> Result<(), Error> {
        if bytes <= self.bytes {
            self.refund(self.bytes - bytes);
        } else {
            self.accounts.charge(bytes - self.bytes)?;
            self.bytes = bytes;
        }
        Ok(())
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        // Refund needs no allocation or admission lock, including during unwind.
        self.refund(self.bytes);
    }
}

/// One aggregate refund authority shared by independently owned domain arenas.
/// Callers refund only completed charges, once per allocation. Borrowing this
/// owner throughout each operation prevents final cleanup during admission.
pub(crate) struct SharedLease {
    accounts: Accounts,
    bytes: AtomicUsize,
}
impl SharedLease {
    pub(crate) fn add(&self, amount: usize) -> Result<(), Error> {
        if amount == 0 {
            return Ok(());
        }
        // Reserve aggregate headroom before parent admission, including when
        // there are no parent budgets. Concurrent refunds must not be lost.
        self.bytes
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |bytes| {
                bytes.checked_add(amount)
            })
            .map_err(|_| limited())?;
        if let Err(error) = self.accounts.charge(amount) {
            self.bytes.fetch_sub(amount, Ordering::Relaxed);
            return Err(error);
        }
        Ok(())
    }
    pub(crate) fn refund(&self, amount: usize) {
        if amount == 0 {
            return;
        }
        let previous = self.bytes.fetch_sub(amount, Ordering::Relaxed);
        assert!(previous >= amount);
        self.accounts.refund(amount);
    }
}
impl Drop for SharedLease {
    fn drop(&mut self) {
        // Exclusive destruction means all in-flight admissions have completed.
        self.accounts.refund(*self.bytes.get_mut());
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
// Index bucket -> shard locking serializes registration/removal; stable slots
// preserve every other index location. A removed allocation has no outstanding
// language capability; root destruction occurs after the last reader. Cleanup
// runs only after the removed record is detached from both locked registries.
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
// Stable slots keep index locations valid when unrelated allocations are released.
enum Slot {
    Occupied(Allocation),
    Vacant(Option<usize>),
}
struct Shard {
    allocations: Vec<Slot>,
    free: Option<usize>,
    live: usize,
    metadata: Lease,
    _charge: Lease,
}
const INDEX_BUCKETS: usize = 16;
struct Location {
    address: usize,
    shard: Arc<Mutex<Shard>>,
    slot: usize,
}
struct Index {
    // Sorted, explicitly charged storage: lookup is logarithmic within one
    // bucket. Insert/remove only move locations, never allocation records.
    locations: Vec<Location>,
    metadata: Lease,
}
impl Index {
    fn find(&self, pointer: NonNull<u8>) -> Result<usize, usize> {
        self.locations
            .binary_search_by_key(&(pointer.as_ptr() as usize), |l| l.address)
    }
}
fn bucket(pointer: NonNull<u8>) -> usize {
    // Use high mixed bits so aligned or adjacent allocations spread across locks.
    let address = (pointer.as_ptr() as u64) >> 4;
    (address.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 60) as usize
}
fn reserve_slot<T>(values: &mut Vec<T>, metadata: &mut Lease) -> Result<(), Error> {
    if values.len() < values.capacity() {
        return Ok(());
    }
    let capacity = values.len().checked_add(1).ok_or_else(limited)?;
    let old = metadata.bytes;
    metadata.resize(
        capacity
            .checked_mul(std::mem::size_of::<T>())
            .ok_or_else(limited)?,
    )?;
    if values.try_reserve_exact(1).is_err() {
        metadata.resize(old).expect("refund allocation metadata");
        return Err(limited());
    }
    Ok(())
}
#[derive(Clone)]
pub(crate) struct Storage(Arc<Root>);
struct Root {
    _charge: Lease,
    accounts: Accounts,
    shards: Mutex<Vec<Arc<Mutex<Shard>>>>,
    // Lock order: one index bucket -> one shard. No indexed operation acquires
    // the root shard-list lock, and no path acquires an index while holding a shard.
    index: [Mutex<Index>; INDEX_BUCKETS],
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
            index: std::array::from_fn(|_| {
                Mutex::new(Index {
                    locations: Vec::new(),
                    metadata: accounts.empty_lease(),
                })
            }),
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
            .find(|s| Arc::strong_count(s) == 1 && lock(s).live == 0)
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
            free: None,
            live: 0,
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
        lock(&self.0.index[bucket(pointer)]).find(pointer).is_ok()
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
            lock(shard).allocations.iter().any(|slot| {
                let Slot::Occupied(allocation) = slot else {
                    return false;
                };
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
        self.with_allocation(pointer, |allocation| {
            allocation.task = super::pool::identity();
        });
    }

    fn with_allocation(&self, pointer: NonNull<u8>, update: impl FnOnce(&mut Allocation)) -> bool {
        let index = lock(&self.0.index[bucket(pointer)]);
        let Ok(position) = index.find(pointer) else {
            return false;
        };
        let location = &index.locations[position];
        let mut shard = lock(&location.shard);
        let Slot::Occupied(allocation) = &mut shard.allocations[location.slot] else {
            unreachable!("registered allocation slot")
        };
        assert_eq!(allocation.pointer, pointer);
        update(allocation);
        true
    }

    /// Install only a thread-safe runtime metadata finalizer after initialization.
    /// No language destructor or local callback may be installed here.
    pub(crate) fn finalize_with(&self, pointer: NonNull<u8>, finalize: unsafe fn(*mut u8)) {
        assert!(
            self.with_allocation(pointer, |allocation| {
                assert!(allocation.finalize.is_none());
                allocation.finalize = Some(finalize);
            }),
            "validated metadata allocation"
        );
    }
    /// # Safety
    /// No capability may access `pointer` after this call, on any worker.
    pub(crate) unsafe fn release(&self, pointer: NonNull<u8>) {
        let allocation = {
            let mut index = lock(&self.0.index[bucket(pointer)]);
            let position = index.find(pointer).expect("validated parallel allocation");
            let location = index.locations.remove(position);
            let mut shard = lock(&location.shard);
            let free = shard.free;
            let Slot::Occupied(allocation) =
                std::mem::replace(&mut shard.allocations[location.slot], Slot::Vacant(free))
            else {
                unreachable!("registered allocation slot")
            };
            assert_eq!(allocation.pointer, pointer);
            shard.free = Some(location.slot);
            shard.live -= 1;
            allocation
        };
        // Index and shard locks are gone before finalizers, deallocation and
        // budget refunds. The removed record carries the sole cleanup authority.
        drop(allocation);
    }
}
impl LocalShard {
    /// Reclaim a private region before the enclosing task tree closes.
    ///
    /// # Safety
    /// No references or owners into this shard may remain live or be exported.
    /// The caller exclusively owns the region and runs on its owning worker.
    pub(crate) unsafe fn release_region(&mut self) {
        // Exclusive region teardown: no owner/reference into these slots may be
        // used concurrently. Retire every index entry before any finalizer runs.
        let count = lock(&self.shard).allocations.len();
        for slot in 0..count {
            let pointer = match &lock(&self.shard).allocations[slot] {
                Slot::Occupied(allocation) => allocation.pointer,
                Slot::Vacant(_) => continue,
            };
            let mut index = lock(&self.root.0.index[bucket(pointer)]);
            let position = index.find(pointer).expect("registered region allocation");
            let location = index.locations.remove(position);
            assert!(Arc::ptr_eq(&location.shard, &self.shard));
            assert_eq!(location.slot, slot);
        }
        let allocations = {
            let mut shard = lock(&self.shard);
            let allocations = std::mem::take(&mut shard.allocations);
            shard.free = None;
            shard.live = 0;
            shard.metadata.resize(0).expect("refund region metadata");
            allocations
        };
        drop(allocations);
    }
    pub(crate) fn bytes(&self) -> usize {
        lock(&self.shard)
            .allocations
            .iter()
            .map(|slot| match slot {
                Slot::Occupied(allocation) => allocation._charge.bytes,
                Slot::Vacant(_) => 0,
            })
            .sum()
    }
    pub(crate) fn allocate(&mut self, size: usize, align: usize) -> Result<NonNull<u8>, Error> {
        let layout = Layout::from_size_align(size.max(1), align).map_err(|_| limited())?;
        let bytes = layout.pad_to_align().size();
        let charge = self.root.accounts().reserve(bytes)?;
        let pointer = NonNull::new(unsafe { alloc(layout) }).ok_or_else(limited)?;
        // Keep unpublished storage in an owning guard. Any metadata admission
        // failure drops it only after the index/shard guards have been released.
        let allocation = Allocation {
            task: super::pool::identity(),
            payload_size: size,
            finalize: None,
            pointer,
            layout,
            _charge: charge,
        };
        let mut index = lock(&self.root.0.index[bucket(pointer)]);
        let position = index.find(pointer).expect_err("new allocation address");
        let Index {
            locations,
            metadata,
        } = &mut *index;
        reserve_slot(locations, metadata)?;
        let mut shard = lock(&self.shard);
        let slot = if let Some(slot) = shard.free {
            let Slot::Vacant(next) = shard.allocations[slot] else {
                unreachable!("free allocation slot")
            };
            shard.free = next;
            shard.allocations[slot] = Slot::Occupied(allocation);
            slot
        } else {
            let Shard {
                allocations,
                metadata,
                ..
            } = &mut *shard;
            reserve_slot(allocations, metadata)?;
            let slot = allocations.len();
            allocations.push(Slot::Occupied(allocation));
            slot
        };
        shard.live += 1;
        index.locations.insert(
            position,
            Location {
                address: pointer.as_ptr() as usize,
                shard: self.shard.clone(),
                slot,
            },
        );
        Ok(pointer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Barrier,
    };

    unsafe fn count_finalizer(pointer: *mut u8) {
        let count = pointer.cast::<Arc<AtomicUsize>>().read();
        count.fetch_add(1, Ordering::Relaxed);
    }
    fn tracked(shard: &mut LocalShard, count: &Arc<AtomicUsize>) -> NonNull<u8> {
        let layout = Layout::new::<Arc<AtomicUsize>>();
        let pointer = shard.allocate(layout.size(), layout.align()).unwrap();
        unsafe {
            pointer
                .cast::<Arc<AtomicUsize>>()
                .as_ptr()
                .write(count.clone())
        };
        shard.root.finalize_with(pointer, count_finalizer);
        pointer
    }

    #[test]
    fn parallel_storage_index_retires_slots_regions_and_detached_owners() {
        let budget = TaskBudget::new(1 << 20);
        let root = Storage::new(vec![budget.clone()]).unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let mut first = root.shard().unwrap();
        let mut second = root.shard().unwrap();
        let owners: Vec<_> = (0..64).map(|_| tracked(&mut first, &count)).collect();
        let other = tracked(&mut second, &count);
        for (i, &pointer) in owners.iter().enumerate() {
            if i % 2 == 0 {
                unsafe { root.release(pointer) }
            }
        }
        for (i, &pointer) in owners.iter().enumerate() {
            assert_eq!(root.contains(pointer), i % 2 != 0);
        }
        // Reuse vacant slots without invalidating locations of the other owners.
        let replacement = tracked(&mut first, &count);
        for (i, &pointer) in owners.iter().enumerate() {
            if i % 2 != 0 {
                assert!(root.contains(pointer));
            }
        }
        unsafe { first.release_region() };
        assert!(!root.contains(replacement));
        assert!(owners.iter().all(|p| !root.contains(*p)));
        assert!(root.contains(other));
        assert_eq!(count.load(Ordering::Relaxed), 65);
        let detached = tracked(&mut first, &count);
        drop((first, second));
        assert!(root.contains(detached));
        unsafe { root.release(detached) };
        assert_eq!(count.load(Ordering::Relaxed), 66);
        // Root fallback also destroys still-registered runtime metadata once.
        drop(root);
        assert_eq!(count.load(Ordering::Relaxed), 67);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn parallel_storage_index_metadata_failures_refund_unpublished_payloads() {
        for prewarm_index in [false, true] {
            let budget = TaskBudget::new(1 << 16);
            let root = Storage::new(vec![budget.clone()]).unwrap();
            let mut shard = root.shard().unwrap();
            if prewarm_index {
                for bucket in &root.0.index {
                    let mut bucket = lock(bucket);
                    let Index {
                        locations,
                        metadata,
                    } = &mut *bucket;
                    reserve_slot(locations, metadata).unwrap();
                }
            }
            // Permit the payload allocation, but not the next index/slot record.
            let held = root
                .accounts()
                .reserve((1 << 16) - budget.used() - 8)
                .unwrap();
            let before = budget.used();
            assert!(shard.allocate(8, 8).is_err());
            assert_eq!(budget.used(), before);
            assert_eq!(lock(&shard.shard).live, 0);
            assert!(root.0.index.iter().all(|b| lock(b).locations.is_empty()));
            drop(held);
            let value = shard.allocate(8, 8).unwrap();
            assert!(root.contains(value));
            unsafe { root.release(value) };
            drop((shard, root));
            assert_eq!(budget.used(), 0);
        }
    }

    #[test]
    fn parallel_storage_lookup_and_release_do_not_lock_unrelated_partitions() {
        let budget = TaskBudget::new(1 << 20);
        let root = Storage::new(vec![budget.clone()]).unwrap();
        let mut shard = root.shard().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let pointer = tracked(&mut shard, &count);
        let unrelated = (bucket(pointer) + 1) % INDEX_BUCKETS;
        let (tx, rx) = mpsc::channel();
        let root_list = lock(&root.0.shards);
        let unrelated_bucket = lock(&root.0.index[unrelated]);
        let worker_root = root.clone();
        let address = pointer.as_ptr() as usize;
        let worker = std::thread::spawn(move || {
            let pointer = NonNull::new(address as *mut u8).unwrap();
            assert!(worker_root.contains(pointer));
            unsafe {
                worker_root.adopt_allocation(pointer);
                worker_root.release(pointer);
            }
            assert!(!worker_root.contains(pointer));
            tx.send(()).unwrap();
        });
        let completed = rx.recv_timeout(std::time::Duration::from_secs(5));
        drop((unrelated_bucket, root_list));
        worker.join().unwrap();
        completed.expect("indexed access must not wait on unrelated partitions");
        assert_eq!(count.load(Ordering::Relaxed), 1);
        drop((shard, root));
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn parallel_storage_finalizers_run_after_all_storage_locks_are_released() {
        struct Probe {
            root: Storage,
            shard: Arc<Mutex<Shard>>,
            count: Arc<AtomicUsize>,
        }
        unsafe fn finalize(pointer: *mut u8) {
            let probe = pointer.cast::<Probe>().read();
            assert!(probe.root.0.shards.try_lock().is_ok());
            assert!(probe.shard.try_lock().is_ok());
            assert!(probe
                .root
                .0
                .index
                .iter()
                .all(|bucket| bucket.try_lock().is_ok()));
            assert!(!probe.root.contains(NonNull::new(pointer).unwrap()));
            let mut nested = probe.root.shard().unwrap();
            let value = nested.allocate(8, 8).unwrap();
            probe.root.release(value);
            probe.count.fetch_add(1, Ordering::Relaxed);
        }
        for region in [false, true] {
            let budget = TaskBudget::new(1 << 20);
            let root = Storage::new(vec![budget.clone()]).unwrap();
            let mut shard = root.shard().unwrap();
            let count = Arc::new(AtomicUsize::new(0));
            let layout = Layout::new::<Probe>();
            let pointer = shard.allocate(layout.size(), layout.align()).unwrap();
            unsafe {
                pointer.cast::<Probe>().as_ptr().write(Probe {
                    root: root.clone(),
                    shard: shard.shard.clone(),
                    count: count.clone(),
                });
                root.finalize_with(pointer, finalize);
                if region {
                    shard.release_region()
                } else {
                    root.release(pointer)
                }
            }
            assert_eq!(count.load(Ordering::Relaxed), 1);
            drop((shard, root));
            assert_eq!(budget.used(), 0);
        }
    }
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
        let index_bytes = || {
            root.0
                .index
                .iter()
                .map(|bucket| lock(bucket).metadata.bytes)
                .sum::<usize>()
        };
        let stable = budget.used() - index_bytes();
        for _ in 0..128 {
            let mut shard = root.shard().unwrap();
            let value = shard.allocate(8, 8).unwrap();
            unsafe {
                root.release(value);
            }
            drop(shard);
            assert_eq!(budget.used() - index_bytes(), stable);
            // Only one allocation is live at a time. Allocator address reuse may
            // warm different buckets, but each caches at most one location.
            assert!(index_bytes() <= INDEX_BUCKETS * std::mem::size_of::<Location>());
            assert_eq!(lock(&root.0.shards).len(), 2);
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
    fn parallel_budget_refunds_do_not_wait_for_admission() {
        let one = TaskBudget::new(100);
        let two = TaskBudget::new(100);
        let accounts = Accounts::new(vec![one.clone(), two.clone()]);
        let mut shrinking = accounts.reserve(40).unwrap();
        let dropped = accounts.reserve(30).unwrap();
        let first = lock(&one.0.admission);
        let second = lock(&two.0.admission);
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            shrinking.resize(25).unwrap();
            shrinking.refund(5);
            shrinking.resize(20).unwrap();
            drop(dropped);
            drop(accounts.reserve(0).unwrap());
            tx.send(shrinking).unwrap();
        });
        let result = rx.recv_timeout(std::time::Duration::from_secs(5));
        // Always release the gates before joining, even if a regression blocks.
        drop((first, second));
        worker.join().unwrap();
        let remaining = result.expect("refund and unchanged usage must not wait for admission");
        assert_eq!((one.used(), two.used()), (20, 20));
        drop(remaining);
        assert_eq!((one.used(), two.used()), (0, 0));
    }

    #[test]
    fn parallel_budget_failed_growth_preserves_every_charge() {
        for count in [0, 1, 2, 5] {
            let mut budgets: Vec<_> = (0..count).map(|_| TaskBudget::new(100)).collect();
            budgets.sort_by_key(|budget| Arc::as_ptr(&budget.0));
            if let Some(last) = budgets.last_mut() {
                Arc::get_mut(&mut last.0).unwrap().limit = 19;
            }
            let accounts = Accounts::new(budgets.clone());
            let mut lease = accounts.reserve(10).unwrap();
            if count > 0 {
                assert_eq!(
                    lease.resize(20).unwrap_err().code(),
                    ErrorCode::ResourceLimit
                );
                assert!(budgets.iter().all(|b| b.used() == 10));
            }
            lease.resize(12).unwrap();
            lease.refund(2);
            lease.resize(3).unwrap();
            assert!(budgets.iter().all(|b| b.used() == 3));
            drop(lease);
            assert!(budgets.iter().all(|b| b.used() == 0));
        }
        let budget = TaskBudget::new(usize::MAX);
        let accounts = Accounts::new(vec![budget.clone()]);
        let mut lease = accounts.reserve(usize::MAX - 1).unwrap();
        assert!(accounts.reserve(2).is_err());
        assert_eq!(budget.used(), usize::MAX - 1);
        lease.resize(usize::MAX).unwrap();
        drop(lease);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn parallel_budget_concurrent_growth_and_refunds_preserve_live_leases() {
        let budgets: Vec<_> = (0..3).map(|_| TaskBudget::new(100)).collect();
        let accounts = Accounts::new(budgets.clone());
        let retained = accounts.reserve(7).unwrap();
        let gate = Barrier::new(4);
        std::thread::scope(|scope| {
            for offset in 0..4 {
                let mut selected = budgets.clone();
                selected.rotate_left(offset % 3);
                selected.truncate(2);
                let accounts = Accounts::new(selected);
                let gate = &gate;
                scope.spawn(move || {
                    gate.wait();
                    for _ in 0..1000 {
                        let mut lease = accounts.reserve(3).unwrap();
                        lease.resize(1).unwrap();
                        lease.resize(4).unwrap();
                        drop(lease);
                    }
                });
            }
        });
        assert!(budgets.iter().all(|b| b.used() == 7));
        drop(retained);
        assert!(budgets.iter().all(|b| b.used() == 0));
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

    #[test]
    fn shared_lease_refunds_while_parent_admission_is_pending() {
        for fail in [false, true] {
            let budget = TaskBudget::new(20);
            let accounts = Accounts::new(vec![budget.clone()]);
            let shared = Arc::new(accounts.shared_lease());
            shared.add(8).unwrap();
            let held = accounts.reserve(if fail { 12 } else { 0 }).unwrap();
            let admission = lock(&budget.0.admission);
            let charging = shared.clone();
            let worker = std::thread::spawn(move || charging.add(12));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while shared.bytes.load(Ordering::Relaxed) != 20 && std::time::Instant::now() < deadline
            {
                std::thread::yield_now();
            }
            let pending = shared.bytes.load(Ordering::Relaxed) == 20;
            let refunding = shared.clone();
            let (tx, rx) = mpsc::channel();
            let refund = std::thread::spawn(move || {
                refunding.refund(8);
                tx.send(()).unwrap();
            });
            let completed = rx.recv_timeout(std::time::Duration::from_secs(5));
            drop(admission);
            let result = worker.join().unwrap();
            refund.join().unwrap();
            assert!(pending, "charge must reach parent admission");
            completed.expect("old charges can be refunded during admission");
            assert_eq!(result.is_err(), fail);
            assert_eq!(
                shared.bytes.load(Ordering::Relaxed),
                if fail { 0 } else { 12 }
            );
            assert_eq!(budget.used(), 12);
            drop(shared);
            assert_eq!(budget.used(), if fail { 12 } else { 0 });
            drop(held);
            assert_eq!(budget.used(), 0);
        }
    }

    #[test]
    fn shared_lease_concurrent_updates_and_final_refund_preserve_other_owners() {
        let budgets: Vec<_> = (0..3).map(|_| TaskBudget::new(100)).collect();
        let accounts = Accounts::new(budgets.clone());
        let other = accounts.reserve(11).unwrap();
        let shared = accounts.shared_lease();
        shared.add(7).unwrap();
        let gate = Barrier::new(4);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    gate.wait();
                    for _ in 0..1000 {
                        shared.add(3).unwrap();
                        shared.refund(1);
                        shared.add(2).unwrap();
                        shared.refund(4);
                    }
                });
            }
        });
        assert_eq!(shared.bytes.load(Ordering::Relaxed), 7);
        assert!(budgets.iter().all(|budget| budget.used() == 18));
        drop(shared);
        assert!(budgets.iter().all(|budget| budget.used() == 11));
        drop(other);
        assert!(budgets.iter().all(|budget| budget.used() == 0));
    }

    #[test]
    fn shared_lease_rejects_overflow_and_partial_parent_charges() {
        let empty = Accounts::new(Vec::new()).shared_lease();
        empty.add(usize::MAX).unwrap();
        assert!(empty.add(1).is_err());
        empty.add(0).unwrap();
        empty.refund(0);
        assert_eq!(empty.bytes.load(Ordering::Relaxed), usize::MAX);
        empty.refund(usize::MAX);
        assert_eq!(empty.bytes.load(Ordering::Relaxed), 0);

        let mut budgets: Vec<_> = (0..3).map(|_| TaskBudget::new(100)).collect();
        budgets.sort_by_key(|budget| Arc::as_ptr(&budget.0));
        Arc::get_mut(&mut budgets[2].0).unwrap().limit = 10;
        let shared = Accounts::new(budgets.clone()).shared_lease();
        shared.add(7).unwrap();
        assert!(shared.add(4).is_err());
        assert_eq!(shared.bytes.load(Ordering::Relaxed), 7);
        assert!(budgets.iter().all(|budget| budget.used() == 7));
        drop(shared);
        assert!(budgets.iter().all(|budget| budget.used() == 0));
    }
}
