//! Executor-local routes optionally own state independently of creating tasks.
use super::commits::{adopt, lease, Stored};
use super::*;
use crate::runtime::{buffer::Buffer, domain::Operation};

struct Entry {
    key: u64,
    handle: DomainHandle,
    token: u64,
    owner: Option<DomainOwner>,
}
pub(super) struct Table {
    entries: Vec<Entry>,
    capacity: usize,
    closed: bool,
    _charge: reads::Charge,
}
struct Owner {
    registry: Weak<RefCell<Domains>>,
    token: u64,
    table: Rc<RefCell<Table>>,
}
impl Drop for Owner {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            registry.borrow_mut().directories.remove(&self.token);
        }
        close_table(&self.table);
    }
}
impl Table {
    fn position(&self, key: u64) -> Result<usize, usize> {
        self.entries.binary_search_by_key(&key, |entry| entry.key)
    }
}
fn close_table(table: &RefCell<Table>) {
    let entries = {
        let mut table = table.borrow_mut();
        table.closed = true;
        std::mem::take(&mut table.entries)
    };
    // Closing domains cancels tasks and may release other directory resources.
    // Never run that cleanup while holding a table or registry borrow.
    drop(entries);
}
fn create(registry: &Rc<RefCell<Domains>>, capacity: u64) -> Result<Owner, Error> {
    let capacity = usize::try_from(capacity).map_err(|_| limited("Directory capacity overflow"))?;
    if capacity == 0 {
        return Err(invalid("Directory capacity must be positive"));
    }
    let mut domains = registry.borrow_mut();
    if domains.shutting_down {
        return Err(invalid("Directory executor is closed"));
    }
    let bytes = capacity
        .checked_mul(std::mem::size_of::<Entry>())
        .and_then(|n| {
            n.checked_add(std::mem::size_of::<Table>() + 4 * std::mem::size_of::<usize>())
        })
        .ok_or_else(|| limited("Directory capacity overflow"))?;
    let charge = reads::Charge::new(domains.budget.clone(), bytes)?;
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(capacity)
        .map_err(|_| limited("Directory allocation failed"))?;
    domains
        .directories
        .try_reserve(1)
        .map_err(|_| limited("Directory registry allocation failed"))?;
    let token = source::next_token()?;
    let table = Rc::new(RefCell::new(Table {
        entries,
        capacity,
        closed: false,
        _charge: charge,
    }));
    domains.directories.insert(token, Rc::downgrade(&table));
    Ok(Owner {
        registry: Rc::downgrade(registry),
        token,
        table,
    })
}
fn table(registry: &Rc<RefCell<Domains>>, token: u64) -> Option<Rc<RefCell<Table>>> {
    let domains = registry.borrow();
    if domains.shutting_down {
        return None;
    }
    domains
        .directories
        .get(&token)
        .and_then(Weak::upgrade)
        .filter(|table| !table.borrow().closed)
}
fn insert(
    registry: &Rc<RefCell<Domains>>,
    directory: u64,
    key: u64,
    expected: Option<u64>,
    token: u64,
) -> u64 {
    insert_entry(registry, directory, key, expected, token, None)
}
fn insert_owned(
    registry: &Rc<RefCell<Domains>>,
    directory: u64,
    key: u64,
    owner: DomainOwner,
) -> u64 {
    let token = {
        let domains = registry.borrow();
        let Ok(domain) = domains.get(owner.handle) else {
            return 4;
        };
        domain.source_token * 2 + 1
    };
    insert_entry(registry, directory, key, None, token, Some(owner))
}
fn prune(registry: &Rc<RefCell<Domains>>, table: &RefCell<Table>) {
    let mut index = 0;
    loop {
        let stale = {
            let table = table.borrow();
            let Some(entry) = table.entries.get(index) else {
                break;
            };
            registry.borrow().get(entry.handle).is_err()
        };
        if stale {
            let entry = table.borrow_mut().entries.remove(index);
            drop(entry);
        } else {
            index += 1;
        }
    }
}
fn insert_entry(
    registry: &Rc<RefCell<Domains>>,
    directory: u64,
    key: u64,
    expected: Option<u64>,
    token: u64,
    owner: Option<DomainOwner>,
) -> u64 {
    let Some(table) = table(registry, directory) else {
        return 1;
    };
    let Ok(handle) = source::handle(registry, token) else {
        return 4;
    };
    if let Some(expected) = expected {
        let mut table = table.borrow_mut();
        let Ok(index) = table.position(key) else {
            return 5;
        };
        if table.entries[index].token / 2 != expected / 2 {
            return 5;
        }
        // A permissions-only replacement must not close the state it still routes.
        let owner = if table.entries[index].token / 2 == token / 2 {
            table.entries[index].owner.take()
        } else {
            owner
        };
        let previous = std::mem::replace(
            &mut table.entries[index],
            Entry {
                key,
                handle,
                token,
                owner,
            },
        );
        drop(table);
        drop(previous);
        return 0;
    }
    // Reclaim dead routes before enforcing capacity, including routes never looked up.
    prune(registry, &table);
    // Retired owners can cancel tasks that own this directory or the new target.
    // Validate again after cleanup before publishing the route.
    if table.borrow().closed {
        return 1;
    }
    if source::handle(registry, token).is_err() {
        return 4;
    }
    let mut table = table.borrow_mut();
    let index = match table.position(key) {
        Ok(_) => return 3,
        Err(index) => index,
    };
    if table.entries.len() == table.capacity {
        return 2;
    }
    table.entries.insert(
        index,
        Entry {
            key,
            handle,
            token,
            owner,
        },
    );
    0
}
fn remove(registry: &Rc<RefCell<Domains>>, directory: u64, key: u64, expected: u64) -> bool {
    let Some(table) = table(registry, directory) else {
        return false;
    };
    let mut table = table.borrow_mut();
    let Ok(index) = table.position(key) else {
        return false;
    };
    if table.entries[index].token / 2 != expected / 2 {
        return false;
    }
    let entry = table.entries.remove(index);
    drop(table);
    drop(entry);
    true
}
fn lookup(
    registry: &Rc<RefCell<Domains>>,
    directory: u64,
    key: u64,
    module: &LoadedModule,
    name: &str,
    writable: bool,
) -> Result<u64, Error> {
    let Some(table) = table(registry, directory) else {
        return Ok(0);
    };
    let mut table = table.borrow_mut();
    let Ok(index) = table.position(key) else {
        return Ok(0);
    };
    let entry = &table.entries[index];
    let domains = registry.borrow();
    let Ok(domain) = domains.get(entry.handle) else {
        drop(domains);
        let entry = table.entries.remove(index);
        drop(table);
        drop(entry);
        return Ok(0);
    };
    if writable && !entry.handle.writable {
        return Ok(0);
    }
    let info = module.instance_info(name)?;
    if !info.domain_entry
        || info.suspends
        || info.signature.params.len() != 1
        || !matches!(&info.signature.params[0],Type::Reference(r) if !r.mutable)
        || !instance::receiver(&info).is_some_and(|ty| code::compatible(ty, &domain.ty))
    {
        return Err(error(
            ErrorCode::TypeMismatch,
            "Directory target does not match the checked state type",
        ));
    }
    Ok(if writable {
        entry.token
    } else {
        entry.token & !1
    })
}
pub(super) fn shutdown(registry: &Rc<RefCell<Domains>>) {
    let tables = std::mem::take(&mut registry.borrow_mut().directories);
    for table in tables.into_values().filter_map(|table| table.upgrade()) {
        close_table(&table);
    }
}
/// # Safety
/// Slots match the compiler-owned operation signature. Resource and text owners
/// remain live throughout this synchronous operation; no callback retains a borrow.
pub(super) unsafe fn poll(
    arena: &mut Arena,
    frame: &mut CpsFrame,
    registry: &Rc<RefCell<Domains>>,
    op: Operation,
) -> Result<u64, Error> {
    let slots = &frame.slots;
    match op {
        Operation::DirectoryCreate => adopt(arena, create(registry, slots[0])?),
        Operation::DirectoryHandle => {
            let lease = lease(slots[0])?;
            let owner = (&*lease.token().cast::<Stored<Owner>>()).value.borrow();
            if !owner.registry.ptr_eq(&Rc::downgrade(registry)) {
                return Err(invalid("Directory belongs to another executor"));
            }
            Ok(owner.token)
        }
        Operation::DirectoryClose => {
            let table = registry
                .borrow_mut()
                .directories
                .remove(&slots[0])
                .and_then(|table| table.upgrade());
            if let Some(table) = table {
                close_table(&table);
            }
            Ok(0)
        }
        Operation::DirectoryInsert => Ok(insert(registry, slots[0], slots[1], None, slots[2])),
        Operation::DirectoryInsertOwned => Ok(insert_owned(
            registry,
            slots[0],
            slots[1],
            source::take_owner(slots[2])?,
        )),
        Operation::DirectoryReplace => Ok(insert(
            registry,
            slots[0],
            slots[1],
            Some(slots[2]),
            slots[3],
        )),
        Operation::DirectoryRemove => Ok(u64::from(remove(registry, slots[0], slots[1], slots[2]))),
        Operation::DirectoryFound => Ok(u64::from(slots[0] != 0)),
        Operation::DirectoryLookup => {
            let (module, _) = source::module(registry, slots[0])?;
            let name = crate::runtime::owned_text::borrow(slots[3] as *const Buffer)?;
            lookup(registry, slots[1], slots[2], &module, name, slots[4] != 0)
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests;
