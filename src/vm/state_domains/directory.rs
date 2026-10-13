//! Executor-local routing capabilities never retain domain roots or code modules.
use super::commits::{adopt, lease, Stored};
use super::*;
use crate::runtime::{buffer::Buffer, domain::Operation};

struct Entry {
    key: u64,
    handle: DomainHandle,
    token: u64,
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
        self.table.borrow_mut().close();
    }
}
impl Table {
    fn close(&mut self) {
        self.closed = true;
        self.entries.clear();
    }
    fn position(&self, key: u64) -> Result<usize, usize> {
        self.entries.binary_search_by_key(&key, |entry| entry.key)
    }
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
    let Some(table) = table(registry, directory) else {
        return 1;
    };
    let Ok(handle) = source::handle(registry, token) else {
        return 4;
    };
    let mut table = table.borrow_mut();
    if let Some(expected) = expected {
        let Ok(index) = table.position(key) else {
            return 5;
        };
        if table.entries[index].token / 2 != expected / 2 {
            return 5;
        }
        table.entries[index] = Entry { key, handle, token };
        return 0;
    }
    // Reclaim dead routes before enforcing capacity, including routes never looked up.
    let domains = registry.borrow();
    table
        .entries
        .retain(|entry| domains.get(entry.handle).is_ok());
    let index = match table.position(key) {
        Ok(_) => return 3,
        Err(index) => index,
    };
    if table.entries.len() == table.capacity {
        return 2;
    }
    table.entries.insert(index, Entry { key, handle, token });
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
    table.entries.remove(index);
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
        table.entries.remove(index);
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
        table.borrow_mut().close();
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
            if let Some(table) = registry
                .borrow_mut()
                .directories
                .remove(&slots[0])
                .and_then(|table| table.upgrade())
            {
                table.borrow_mut().close();
            }
            Ok(0)
        }
        Operation::DirectoryInsert => Ok(insert(registry, slots[0], slots[1], None, slots[2])),
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
