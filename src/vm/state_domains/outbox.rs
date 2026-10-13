//! Bounded byte batches become visible only with candidate root publication.
use super::commits::{adopt, lease, Status, Stored};
use super::*;
use crate::runtime::{buffer::Buffer, domain::Operation};
use std::collections::VecDeque;

const MAX_BATCH_MESSAGES: usize = 256;
struct Message {
    bytes: Vec<u8>,
    _charge: reads::Charge,
}
pub(super) struct Mailbox {
    registry: u64,
    max_messages: usize,
    max_bytes: usize,
    bytes: usize,
    closed: bool,
    messages: VecDeque<Message>,
    waiter: Option<TaskWaker>,
    _charge: reads::Charge,
}
impl Mailbox {
    fn close(&mut self) {
        self.closed = true;
        self.messages.clear();
        self.bytes = 0;
        if let Some(waker) = &self.waiter {
            waker.wake();
        }
    }
}
struct Owner {
    registry: Weak<RefCell<Domains>>,
    token: u64,
    queue: Rc<RefCell<Mailbox>>,
}
impl Drop for Owner {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            registry.borrow_mut().outboxes.remove(&self.token);
        }
        self.queue.borrow_mut().close();
    }
}
struct Staged {
    token: u64,
    message: Message,
}
pub(super) struct Batch {
    registry: u64,
    max_messages: usize,
    max_bytes: usize,
    bytes: usize,
    failure: Option<Status>,
    messages: Vec<Staged>,
    // Allocated before staging. Preflight and publication cannot grow this vector.
    targets: Vec<Rc<RefCell<Mailbox>>>,
    _charge: reads::Charge,
}
fn size(value: u64) -> Result<usize, Error> {
    usize::try_from(value).map_err(|_| limited("Outbox size overflow"))
}
fn vector<T>(capacity: usize) -> Result<Vec<T>, Error> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| limited("Outbox allocation failed"))?;
    Ok(values)
}
fn create(registry: &Rc<RefCell<Domains>>, messages: u64, bytes: u64) -> Result<Owner, Error> {
    let max_messages = size(messages)?;
    let max_bytes = size(bytes)?;
    if max_messages == 0 {
        return Err(invalid("Outbox message capacity must be positive"));
    }
    let mut domains = registry.borrow_mut();
    if domains.shutting_down {
        return Err(invalid("Outbox executor is closed"));
    }
    let overhead = max_messages
        .checked_mul(std::mem::size_of::<Message>())
        .and_then(|n| {
            n.checked_add(
                std::mem::size_of::<Mailbox>()
                    + std::mem::size_of::<Wait>()
                    + 4 * std::mem::size_of::<usize>(),
            )
        })
        .ok_or_else(|| limited("Outbox capacity overflow"))?;
    let charge = reads::Charge::new(domains.budget.clone(), overhead)?;
    let mut queue = VecDeque::new();
    queue
        .try_reserve_exact(max_messages)
        .map_err(|_| limited("Outbox allocation failed"))?;
    domains
        .outboxes
        .try_reserve(1)
        .map_err(|_| limited("Outbox registry allocation failed"))?;
    let token = source::next_token()?;
    let queue = Rc::new(RefCell::new(Mailbox {
        registry: domains.identity,
        max_messages,
        max_bytes,
        bytes: 0,
        closed: false,
        messages: queue,
        waiter: None,
        _charge: charge,
    }));
    domains.outboxes.insert(token, Rc::downgrade(&queue));
    Ok(Owner {
        registry: Rc::downgrade(registry),
        token,
        queue,
    })
}
fn batch(registry: &Rc<RefCell<Domains>>, messages: u64, bytes: u64) -> Result<Batch, Error> {
    let max_messages = size(messages)?;
    let max_bytes = size(bytes)?;
    if max_messages == 0 || max_messages > MAX_BATCH_MESSAGES {
        return Err(invalid("Outbox batch requires 1 to 256 message slots"));
    }
    let domains = registry.borrow();
    if domains.shutting_down {
        return Err(invalid("Outbox executor is closed"));
    }
    let charge = reads::Charge::new(
        domains.budget.clone(),
        std::mem::size_of::<Batch>()
            + max_messages
                * (std::mem::size_of::<Staged>() + std::mem::size_of::<Rc<RefCell<Mailbox>>>()),
    )?;
    Ok(Batch {
        registry: domains.identity,
        max_messages,
        max_bytes,
        bytes: 0,
        failure: None,
        messages: vector(max_messages)?,
        targets: vector(max_messages)?,
        _charge: charge,
    })
}
impl Batch {
    fn stage(&mut self, domains: &Domains, token: u64, bytes: &[u8]) -> Result<u64, Error> {
        if self.registry != domains.identity {
            return Err(invalid("Outbox batch belongs to another executor"));
        }
        if self.failure.is_none() {
            if domains.shutting_down
                || domains
                    .outboxes
                    .get(&token)
                    .and_then(Weak::upgrade)
                    .is_none_or(|q| q.borrow().closed)
            {
                self.failure = Some(Status::OutboxClosed);
            } else if self.messages.len() == self.max_messages
                || bytes.len() > self.max_bytes - self.bytes
            {
                self.failure = Some(Status::OutboxFull);
            }
        }
        if let Some(status) = self.failure {
            return Ok(if status == Status::OutboxFull { 1 } else { 2 });
        }
        let charge = reads::Charge::new(domains.budget.clone(), bytes.len())?;
        let mut copy = vector(bytes.len())?;
        copy.extend_from_slice(bytes);
        self.messages.push(Staged {
            token,
            message: Message {
                bytes: copy,
                _charge: charge,
            },
        });
        self.bytes += bytes.len();
        Ok(0)
    }
    pub(super) fn prepare(&mut self, domains: &Domains) -> Result<(), Status> {
        if self.registry != domains.identity {
            return Err(Status::InvalidAccess);
        }
        if let Some(status) = self.failure {
            return Err(status);
        }
        if domains.shutting_down {
            return Err(Status::OutboxClosed);
        }
        self.targets.clear();
        for (index, message) in self.messages.iter().enumerate() {
            let target = domains
                .outboxes
                .get(&message.token)
                .and_then(Weak::upgrade)
                .ok_or(Status::OutboxClosed)?;
            {
                let queue = target.borrow();
                if queue.closed {
                    return Err(Status::OutboxClosed);
                }
                // At most 256 entries; include every earlier message to this target,
                // including empty messages, without allocating a reservation map.
                let previous = self.messages[..index]
                    .iter()
                    .filter(|m| m.token == message.token);
                let (count, bytes) = previous
                    .fold((1usize, message.message.bytes.len()), |(n, b), m| {
                        (n + 1, b + m.message.bytes.len())
                    });
                if count > queue.max_messages - queue.messages.len()
                    || bytes > queue.max_bytes - queue.bytes
                {
                    return Err(Status::OutboxFull);
                }
            }
            self.targets.push(target);
        }
        Ok(())
    }
    // Called in the same registry turn as prepare, with no intervening callbacks.
    pub(super) fn publish(&mut self) {
        for (message, target) in self.messages.drain(..).zip(&self.targets) {
            let mut queue = target.borrow_mut();
            queue.bytes += message.message.bytes.len();
            queue.messages.push_back(message.message);
        }
    }
    // Notifications are deliberately outside the root/message publication interval.
    pub(super) fn wake(&self) {
        for target in &self.targets {
            if let Some(waker) = &target.borrow().waiter {
                waker.wake();
            }
        }
    }
}
pub(super) fn shutdown(registry: &Rc<RefCell<Domains>>) {
    let queues: Vec<_> = registry
        .borrow_mut()
        .outboxes
        .drain()
        .filter_map(|(_, q)| q.upgrade())
        .collect();
    for queue in queues {
        queue.borrow_mut().close();
    }
}
struct Wait {
    queue: Rc<RefCell<Mailbox>>,
}
impl Drop for Wait {
    fn drop(&mut self) {
        self.queue.borrow_mut().waiter = None;
    }
}
unsafe extern "C" fn stop(frame: *mut CpsFrame, _: u32) {
    let context = std::mem::replace(&mut (*frame).host_context, std::ptr::null_mut());
    (*frame).host_stop = None;
    if !context.is_null() {
        drop(Box::from_raw(context.cast::<Wait>()));
    }
}
/// # Safety
/// Slots obey the compiler-checked signature. Resource and byte buffer owners,
/// scalar output places, and the arena stay live until completion or stop.
pub(super) unsafe fn poll(
    arena: &mut Arena,
    frame: &mut CpsFrame,
    registry: &Rc<RefCell<Domains>>,
    op: Operation,
) -> Result<Option<u64>, Error> {
    let result = poll_inner(arena, frame, registry, op);
    if result.is_err() && op == Operation::OutboxReceive {
        stop(frame, 0);
    }
    result
}
unsafe fn poll_inner(
    arena: &mut Arena,
    frame: &mut CpsFrame,
    registry: &Rc<RefCell<Domains>>,
    op: Operation,
) -> Result<Option<u64>, Error> {
    match op {
        Operation::OutboxCreate => {
            adopt(arena, create(registry, frame.slots[0], frame.slots[1])?).map(Some)
        }
        Operation::OutboxBatch => adopt(
            arena,
            Some(batch(registry, frame.slots[0], frame.slots[1])?),
        )
        .map(Some),
        Operation::OutboxClose => {
            let queue = registry
                .borrow()
                .outboxes
                .get(&frame.slots[0])
                .and_then(Weak::upgrade);
            if let Some(queue) = queue {
                queue.borrow_mut().close();
            }
            Ok(Some(0))
        }
        Operation::OutboxStage => {
            let lease = lease(frame.slots[0])?;
            let stored = &*lease.token().cast::<Stored<Option<Batch>>>();
            let mut batch = stored.value.borrow_mut();
            let batch = batch
                .as_mut()
                .ok_or_else(|| invalid("Outbox batch was already attached"))?;
            let bytes = (&*(frame.slots[2] as *const Buffer)).bytes()?;
            batch
                .stage(&registry.borrow(), frame.slots[1], bytes)
                .map(Some)
        }
        Operation::OutboxAttach | Operation::OutboxAttachPair | Operation::OutboxAttachSet => {
            let lease = lease(frame.slots[1])?;
            let stored = &*lease.token().cast::<Stored<Option<Batch>>>();
            let mut batch = stored.value.borrow_mut();
            if batch
                .as_ref()
                .is_none_or(|b| b.registry != registry.borrow().identity)
            {
                return Err(invalid(
                    "Outbox batch is consumed or belongs to another executor",
                ));
            }
            commits::attach_outbox(frame.slots[0], op, &mut batch)?;
            Ok(Some(0))
        }
        Operation::OutboxMessageClosed | Operation::OutboxMessageBytes => {
            let lease = lease(frame.slots[0])?;
            let stored = &*lease.token().cast::<Stored<Option<Message>>>();
            if op == Operation::OutboxMessageClosed {
                return Ok(Some(u64::from(stored.value.borrow().is_none())));
            }
            let mut message = stored.value.borrow_mut();
            let bytes = &message
                .as_ref()
                .ok_or_else(|| invalid("Outbox message was already consumed or closed"))?
                .bytes;
            let output = Buffer::copy_bytes(arena, bytes)?;
            message.take();
            Ok(Some(output.as_ptr() as usize as u64))
        }
        Operation::OutboxHandle | Operation::OutboxReceive => {
            let lease = lease(frame.slots[0])?;
            let stored = &*lease.token().cast::<Stored<Owner>>();
            let owner = stored.value.borrow();
            if owner.queue.borrow().registry != registry.borrow().identity {
                return Err(invalid("Outbox belongs to another executor"));
            }
            if op == Operation::OutboxHandle {
                return Ok(Some(owner.token));
            }
            let mut queue = owner.queue.borrow_mut();
            if queue.closed || !queue.messages.is_empty() {
                // Allocate result ownership before removing the queue's front.
                let output = adopt(arena, None::<Message>)?;
                let result_lease = commits::lease(output)?;
                let result = &*result_lease.token().cast::<Stored<Option<Message>>>();
                if let Some(message) = queue.messages.pop_front() {
                    queue.bytes -= message.bytes.len();
                    *result.value.borrow_mut() = Some(message);
                }
                drop(queue);
                stop(frame, 0);
                return Ok(Some(output));
            }
            let waker =
                current_waker().ok_or_else(|| invalid("Outbox receive requires a task waker"))?;
            if frame.host_context.is_null() {
                if queue.waiter.is_some() {
                    return Err(invalid("Outbox already has a receiver"));
                }
                frame.host_context = Box::into_raw(Box::new(Wait {
                    queue: owner.queue.clone(),
                }))
                .cast();
                frame.host_stop = Some(stop);
            }
            queue.waiter = Some(waker);
            frame.wake = None;
            Ok(None)
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests;
