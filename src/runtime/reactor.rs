//! Owner-thread readiness collection. Notifications only enqueue root wakes.
use super::{executor::TaskWaker, resource::ResourceEvent};
use crate::{Error, ErrorCode, ErrorPhase};
use mio::{event::Source, Events, Poll, Registry, Token};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet, VecDeque},
    io,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

pub use mio::{net, Interest};

const WAKE: Token = Token(0);
const MAX_REGISTRATIONS: usize = 4096;
const EVENT_BATCH: usize = 256;

fn failure(message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, ErrorCode::HostError, message)
}
fn os_error(error: io::Error) -> Error {
    failure(format!("Readiness driver: {error}"))
}
struct Waiter {
    waker: TaskWaker,
    event: Option<ResourceEvent>,
}
#[derive(Default)]
struct Ready {
    tokens: BTreeSet<usize>,
    queue: VecDeque<usize>,
}
struct Core {
    poll: RefCell<Poll>,
    registry: Registry,
    events: RefCell<Events>,
    signal: Arc<mio::Waker>,
    waiters: RefCell<BTreeMap<usize, Waiter>>,
    ready: RefCell<Ready>,
    routes: RefCell<BTreeMap<usize, [Option<Direction>; 2]>>,
    batch: usize,
    next: Cell<usize>,
    limit: usize,
    sockets: Cell<usize>,
}

/// A bounded reactor, shared only on its owning thread. One kernel poll collects
/// at most the event batch size; excess readiness remains for the next drive.
#[derive(Clone)]
pub struct Reactor(Rc<Core>);
#[derive(Debug, Default)]
pub struct IoReport {
    pub events: usize,
    pub notified: usize,
}
impl Reactor {
    /// The admission limit also bounds TCP sockets, including idle owners and
    /// in-flight bind/connect/accept reservations.
    pub fn new(max_registrations: usize, event_batch: usize) -> Result<Self, Error> {
        if max_registrations == 0 || event_batch == 0 {
            return Err(failure("Readiness limits must be positive"));
        }
        let poll = Poll::new().map_err(os_error)?;
        let registry = poll.registry().try_clone().map_err(os_error)?;
        let signal = Arc::new(mio::Waker::new(&registry, WAKE).map_err(os_error)?);
        Ok(Self(Rc::new(Core {
            poll: RefCell::new(poll),
            registry,
            events: RefCell::new(Events::with_capacity(event_batch)),
            signal,
            waiters: RefCell::new(BTreeMap::new()),
            ready: RefCell::new(Ready::default()),
            routes: RefCell::new(BTreeMap::new()),
            batch: event_batch,
            next: Cell::new(1),
            limit: max_registrations,
            sockets: Cell::new(0),
        })))
    }
    pub(crate) fn for_executor() -> Result<Self, Error> {
        Self::new(MAX_REGISTRATIONS, EVENT_BATCH)
    }
    pub(crate) fn signal(&self) -> Arc<mio::Waker> {
        self.0.signal.clone()
    }
    pub fn registration_count(&self) -> usize {
        self.0.waiters.borrow().len()
    }
    // Count idle sockets as well as pending creation/acceptance. Registration
    // limits alone do not constrain descriptors retained in language containers.
    pub(crate) fn reserve_socket(&self) -> Result<SocketPermit, Error> {
        let count = self.0.sockets.get();
        if count >= self.0.limit {
            return Err(Error::new_with_code(
                ErrorPhase::Runtime,
                ErrorCode::ResourceLimit,
                "TCP socket limit exceeded",
            ));
        }
        self.0.sockets.set(count + 1);
        Ok(SocketPermit {
            reactor: self.clone(),
        })
    }
    #[cfg(test)]
    pub(crate) fn socket_count(&self) -> usize {
        self.0.sockets.get()
    }
    fn notify(&self, token: usize) -> bool {
        let waiters = self.0.waiters.borrow();
        let Some(waiter) = waiters.get(&token) else {
            return false;
        };
        if waiter.event.as_ref().is_some_and(|e| !e.is_active()) {
            return false;
        }
        let mut ready = self.0.ready.borrow_mut();
        if ready.tokens.insert(token) {
            ready.queue.push_back(token);
        }
        waiter.waker.wake();
        true
    }
    fn clear_ready(&self, token: usize) {
        let mut ready = self.0.ready.borrow_mut();
        if ready.tokens.remove(&token) {
            ready.queue.retain(|id| *id != token);
        }
    }
    /// Collect readiness without running script code. Use Duration::ZERO in an
    /// application's frame loop. None permits an interruptible blocking wait.
    /// Readiness is a hint: retry I/O and wait again after WouldBlock. A host
    /// that stops before WouldBlock must preserve progress and queue another
    /// wake, or finish and attempt I/O before registering its next operation.
    pub fn poll(&self, timeout: Option<Duration>) -> Result<IoReport, Error> {
        let mut poll = self
            .0
            .poll
            .try_borrow_mut()
            .map_err(|_| failure("Readiness driver is already polling"))?;
        let mut events = self.0.events.borrow_mut();
        // Root instruction budgets can yield before the notified child is polled.
        // Keep readiness latched until that operation rearms or stops. Replay one
        // bounded, rotating batch so a consumed root wake cannot lose an edge.
        let retained: Vec<_> = {
            let mut ready = self.0.ready.borrow_mut();
            (0..self.0.batch.min(ready.queue.len()))
                .map(|_| {
                    let token = ready.queue.pop_front().unwrap();
                    ready.queue.push_back(token);
                    token
                })
                .collect()
        };
        let mut notified = 0;
        for token in &retained {
            if self.notify(*token) {
                notified += 1;
            } else {
                self.clear_ready(*token);
            }
        }
        let timeout = if notified == 0 {
            timeout
        } else {
            Some(Duration::ZERO)
        };
        match poll.poll(&mut events, timeout) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                return Ok(IoReport::default());
            }
            Err(error) => return Err(os_error(error)),
        }
        let mut report = IoReport {
            notified,
            ..Default::default()
        };
        for event in events.iter() {
            report.events += 1;
            let route = self.0.routes.borrow().get(&event.token().0).copied();
            for direction in route.into_iter().flatten().flatten() {
                if (event.is_error()
                    || direction.interest.is_readable()
                        && (event.is_readable() || event.is_read_closed())
                    || direction.interest.is_writable()
                        && (event.is_writable() || event.is_write_closed()))
                    && self.notify(direction.token)
                {
                    report.notified += 1;
                }
            }
        }
        Ok(report)
    }
}

pub(crate) struct SocketPermit {
    reactor: Reactor,
}
impl Drop for SocketPermit {
    fn drop(&mut self) {
        let count = &self.reactor.0.sockets;
        count.set(count.get() - 1);
    }
}

#[derive(Clone, Copy)]
struct Direction {
    token: usize,
    interest: Interest,
}
fn overlaps(a: Interest, b: Interest) -> bool {
    a.is_readable() && b.is_readable() || a.is_writable() && b.is_writable()
}
struct SourceState<S> {
    source: S,
    token: Option<usize>,
    directions: [Option<Direction>; 2],
    poisoned: bool,
    // Mio sources stay bound to one Poll; keep it alive between operations.
    reactor: Option<Reactor>,
}
impl<S: Source> SourceState<S> {
    fn interest(&self) -> Option<Interest> {
        self.directions
            .iter()
            .flatten()
            .map(|d| d.interest)
            .reduce(Interest::add)
    }
    fn publish(&self, reactor: &Reactor) {
        if let Some(token) = self.token {
            reactor.0.routes.borrow_mut().insert(token, self.directions);
        }
    }
    fn poison(&mut self, reactor: &Reactor) {
        self.poisoned = true;
        if let Some(token) = self.token.take() {
            reactor.0.routes.borrow_mut().remove(&token);
            let _ = reactor.0.registry.deregister(&mut self.source);
        }
        // Remaining operations must observe the failure even without a new edge.
        for direction in self.directions.iter().flatten() {
            reactor.notify(direction.token);
        }
    }
}
/// Own a nonblocking Mio source. Active registrations retain the source until
/// synchronous deregistration, without borrowing language storage or raw fds.
pub struct IoSource<S>(Rc<RefCell<SourceState<S>>>);
impl<S: Source> IoSource<S> {
    pub(crate) fn allocation_size() -> usize {
        std::mem::size_of::<RefCell<SourceState<S>>>() + 2 * std::mem::size_of::<usize>()
    }
    pub fn new(source: S) -> Self {
        Self(Rc::new(RefCell::new(SourceState {
            source,
            token: None,
            directions: [None; 2],
            poisoned: false,
            reactor: None,
        })))
    }
    /// All I/O must go through the Mio source, including on Windows.
    pub fn with_io<T>(&self, operation: impl FnOnce(&mut S) -> T) -> T {
        operation(&mut self.0.borrow_mut().source)
    }
    /// Capture the current executor root. Drop the returned registration during
    /// stop, before releasing the operation's resource lease and byte storage.
    /// After registration, attempt I/O before returning Pending; readiness left
    /// from an earlier operation must not depend on another kernel edge.
    pub fn register(
        &self,
        interest: Interest,
        event: Option<ResourceEvent>,
    ) -> Result<IoRegistration<S>, Error> {
        let reactor = super::executor::current_reactor()?;
        let waker = super::executor::current_waker()
            .ok_or_else(|| failure("I/O registration requires a task executor"))?;
        self.register_with(&reactor, interest, event, waker)
    }
    fn register_with(
        &self,
        reactor: &Reactor,
        interest: Interest,
        event: Option<ResourceEvent>,
        waker: TaskWaker,
    ) -> Result<IoRegistration<S>, Error> {
        if event.as_ref().is_some_and(|e| !e.is_active()) {
            return Err(failure("Cannot register an inactive resource event"));
        }
        if reactor.registration_count() >= reactor.0.limit {
            return Err(Error::new_with_code(
                ErrorPhase::Runtime,
                ErrorCode::ResourceLimit,
                "Readiness registration limit exceeded",
            ));
        }
        let mut state = self.0.borrow_mut();
        if state.poisoned
            || state
                .directions
                .iter()
                .flatten()
                .any(|d| overlaps(d.interest, interest))
        {
            return Err(failure(
                "I/O source is already registered or cannot be reused",
            ));
        }
        if state
            .reactor
            .as_ref()
            .is_some_and(|owner| !Rc::ptr_eq(&owner.0, &reactor.0))
        {
            return Err(failure("I/O source belongs to another readiness driver"));
        }
        let token = reactor.0.next.get();
        let next = token
            .checked_add(1)
            .ok_or_else(|| failure("Readiness identities exhausted"))?;
        // Consume identities even when OS registration fails; never reuse them.
        reactor.0.next.set(next);
        let slot = state
            .directions
            .iter()
            .position(Option::is_none)
            .ok_or_else(|| failure("I/O source has no free direction"))?;
        if let Some(kernel) = state.token {
            let combined = state.interest().unwrap().add(interest);
            if let Err(error) =
                reactor
                    .0
                    .registry
                    .reregister(&mut state.source, Token(kernel), combined)
            {
                state.poison(reactor);
                return Err(os_error(error));
            }
        } else {
            if let Err(error) =
                reactor
                    .0
                    .registry
                    .register(&mut state.source, Token(token), interest)
            {
                state.poisoned = true;
                state.reactor = Some(reactor.clone());
                let _ = reactor.0.registry.deregister(&mut state.source);
                return Err(os_error(error));
            }
            state.token = Some(token);
        }
        state.directions[slot] = Some(Direction { token, interest });
        state.reactor = Some(reactor.clone());
        state.publish(reactor);
        reactor
            .0
            .waiters
            .borrow_mut()
            .insert(token, Waiter { waker, event });
        Ok(IoRegistration {
            source: self.0.clone(),
            reactor: reactor.clone(),
            token,
            active: true,
        })
    }
}

/// One direction of readiness. Its destructor removes the notification
/// identity before deregistering the still-live source. Tokens never wrap/reuse.
pub struct IoRegistration<S: Source> {
    source: Rc<RefCell<SourceState<S>>>,
    reactor: Reactor,
    token: usize,
    active: bool,
}
impl<S: Source> IoRegistration<S> {
    /// Whether the driver has collected an unconsumed readiness hint.
    pub fn is_ready(&self) -> bool {
        self.active && self.reactor.0.ready.borrow().tokens.contains(&self.token)
    }
    /// Rearm after WouldBlock or update the operation's readiness interest.
    pub fn rearm(&self, interest: Interest) -> Result<(), Error> {
        if !self.active {
            return Err(failure("Readiness registration has stopped"));
        }
        let mut state = self.source.borrow_mut();
        if state.poisoned {
            return Err(failure("I/O source cannot be reused"));
        }
        let slot = state
            .directions
            .iter()
            .position(|d| d.is_some_and(|d| d.token == self.token))
            .ok_or_else(|| failure("Readiness registration has stopped"))?;
        if state
            .directions
            .iter()
            .flatten()
            .any(|d| d.token != self.token && overlaps(d.interest, interest))
        {
            return Err(failure("I/O direction is already registered"));
        }
        let combined = state
            .directions
            .iter()
            .flatten()
            .filter(|d| d.token != self.token)
            .fold(interest, |all, d| all.add(d.interest));
        let kernel = state.token.unwrap();
        self.reactor.clear_ready(self.token);
        if let Err(error) =
            self.reactor
                .0
                .registry
                .reregister(&mut state.source, Token(kernel), combined)
        {
            state.poison(&self.reactor);
            return Err(os_error(error));
        }
        state.directions[slot] = Some(Direction {
            token: self.token,
            interest,
        });
        state.publish(&self.reactor);
        Ok(())
    }
    /// Explicit synchronous stop with error reporting. Drop also stops; a failed
    /// deregistration poisons the source, and its old token remains invalid.
    pub fn stop(&mut self) -> Result<(), Error> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        self.reactor.0.waiters.borrow_mut().remove(&self.token);
        self.reactor.clear_ready(self.token);
        let mut state = self.source.borrow_mut();
        for direction in &mut state.directions {
            if direction.is_some_and(|d| d.token == self.token) {
                *direction = None;
            }
        }
        if state.poisoned {
            return Err(failure("I/O source cannot be reused"));
        }
        let kernel = state.token.unwrap();
        let result = if let Some(interest) = state.interest() {
            self.reactor
                .0
                .registry
                .reregister(&mut state.source, Token(kernel), interest)
        } else {
            self.reactor.0.routes.borrow_mut().remove(&kernel);
            state.token = None;
            self.reactor.0.registry.deregister(&mut state.source)
        };
        if result.is_err() {
            state.poison(&self.reactor);
        } else {
            state.publish(&self.reactor);
        }
        result.map_err(os_error)
    }
}
impl<S: Source> Drop for IoRegistration<S> {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
#[path = "reactor_tests.rs"]
mod tests;
