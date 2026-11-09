//! Shared readiness collection. Notifications only enqueue root wakes.
use super::{executor::TaskWaker, resource::ResourceEvent};
use crate::{Error, ErrorCode, ErrorPhase};
use mio::{event::Source, Events, Poll, Registry, Token};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io,
    sync::{Arc, Mutex, MutexGuard, TryLockError},
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
#[derive(Clone)]
pub(crate) enum Wake {
    Local(TaskWaker),
    Parallel(super::parallel::pool::IoWaker),
}
impl From<TaskWaker> for Wake {
    fn from(waker: TaskWaker) -> Self {
        Self::Local(waker)
    }
}
impl Wake {
    fn wake(&self) {
        match self {
            Self::Local(waker) => waker.wake(),
            Self::Parallel(waker) => waker.wake(),
        }
    }
}
struct Waiter {
    waker: Wake,
    event: Option<ResourceEvent>,
}
#[derive(Default)]
struct Ready {
    tokens: BTreeSet<usize>,
    queue: VecDeque<usize>,
}
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}
struct Driver {
    poll: Poll,
    events: Events,
}
struct Table {
    waiters: BTreeMap<usize, Waiter>,
    ready: Ready,
    routes: BTreeMap<usize, [Option<Direction>; 2]>,
    next: usize,
    sockets: usize,
    closed: bool,
}
impl Table {
    fn latch(&mut self, token: usize, wakes: &mut Notifications) -> bool {
        if self.closed {
            return false;
        }
        let Some(waiter) = self.waiters.get(&token) else {
            return false;
        };
        if waiter.event.as_ref().is_some_and(|e| !e.is_active()) {
            return false;
        }
        if self.ready.tokens.insert(token) {
            self.ready.queue.push_back(token);
        }
        wakes.0.push(waiter.waker.clone());
        true
    }
    fn clear_ready(&mut self, token: usize) {
        if self.ready.tokens.remove(&token) {
            self.ready.queue.retain(|id| *id != token);
        }
    }
}
// Declare before acquiring source/table guards so every return path delivers
// notifications only after releasing both locks, including OS error paths.
#[derive(Default)]
struct Notifications(Vec<Wake>);
impl Drop for Notifications {
    fn drop(&mut self) {
        for waker in &self.0 {
            waker.wake();
        }
    }
}
struct Core {
    driver: Mutex<Driver>,
    #[cfg(test)]
    before_poll: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    registry: Registry,
    signal: Arc<mio::Waker>,
    table: Mutex<Table>,
    batch: usize,
    limit: usize,
}

/// A bounded, thread-safe reactor. Sources stay bound to the same Poll even
/// when their owner changes threads. Exactly one caller may drive poll at once;
/// registration, I/O and stop do not acquire the driver lock.
#[derive(Clone)]
pub struct Reactor(Arc<Core>);
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
        Ok(Self(Arc::new(Core {
            driver: Mutex::new(Driver {
                poll,
                events: Events::with_capacity(event_batch),
            }),
            #[cfg(test)]
            before_poll: Mutex::new(None),
            registry,
            signal,
            table: Mutex::new(Table {
                waiters: BTreeMap::new(),
                ready: Ready::default(),
                routes: BTreeMap::new(),
                next: 1,
                sockets: 0,
                closed: false,
            }),
            batch: event_batch,
            limit: max_registrations,
        })))
    }
    pub(crate) fn for_executor() -> Result<Self, Error> {
        Self::new(MAX_REGISTRATIONS, EVENT_BATCH)
    }
    pub(crate) fn signal(&self) -> Arc<mio::Waker> {
        self.0.signal.clone()
    }
    /// Active operation leases, including closed operations awaiting stop.
    pub fn registration_count(&self) -> usize {
        lock(&self.0.table).waiters.len()
    }
    /// Refuse new operations and revoke all readiness routes before waking
    /// their owners. Interrupt an in-flight blocking poll. Owners must still
    /// stop/drop their registrations to synchronously deregister live sources.
    pub fn request_close(&self) {
        let mut wakes = Notifications::default();
        {
            let mut table = lock(&self.0.table);
            if table.closed {
                return;
            }
            table.closed = true;
            table.routes.clear();
            table.ready = Ready::default();
            wakes
                .0
                .extend(table.waiters.values().map(|w| w.waker.clone()));
        }
        let _ = self.0.signal.wake();
    }
    // Count idle sockets as well as pending creation/acceptance. Registration
    // limits alone do not constrain descriptors retained in language containers.
    pub(crate) fn reserve_socket(&self) -> Result<SocketPermit, Error> {
        let mut table = lock(&self.0.table);
        if table.closed {
            return Err(failure("Readiness driver is closed"));
        }
        if table.sockets >= self.0.limit {
            return Err(Error::new_with_code(
                ErrorPhase::Runtime,
                ErrorCode::ResourceLimit,
                "TCP socket limit exceeded",
            ));
        }
        table.sockets += 1;
        Ok(SocketPermit {
            reactor: self.clone(),
        })
    }
    #[cfg(test)]
    pub(crate) fn socket_count(&self) -> usize {
        lock(&self.0.table).sockets
    }
    #[cfg(test)]
    fn notify(&self, token: usize) -> bool {
        let mut wakes = Notifications::default();
        let notified = lock(&self.0.table).latch(token, &mut wakes);
        notified
    }
    /// Collect readiness without running script code. Use Duration::ZERO in an
    /// application's frame loop. None permits an interruptible blocking wait.
    /// Readiness is a hint: retry I/O and wait again after WouldBlock. A host
    /// that stops before WouldBlock must preserve progress and queue another
    /// wake, or finish and attempt I/O before registering its next operation.
    pub fn poll(&self, timeout: Option<Duration>) -> Result<IoReport, Error> {
        self.collect(timeout, true)
    }
    // Worker generations retain wakes across instruction slices. A dedicated
    // driver must not spin replaying a ready operation while its worker runs.
    pub(crate) fn poll_events(&self) -> Result<IoReport, Error> {
        self.collect(None, false)
    }
    pub(crate) fn is_closed(&self) -> bool {
        lock(&self.0.table).closed
    }
    fn collect(&self, timeout: Option<Duration>, replay: bool) -> Result<IoReport, Error> {
        let mut wakes = Notifications::default();
        let mut driver = match self.0.driver.try_lock() {
            Ok(driver) => driver,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => {
                return Err(failure("Readiness driver is already polling"));
            }
        };
        // Root instruction budgets can yield before the notified child is polled.
        // Keep readiness latched until that operation rearms or stops. Replay one
        // bounded, rotating batch so a consumed root wake cannot lose an edge.
        let notified = {
            let mut table = lock(&self.0.table);
            if table.closed {
                return Err(failure("Readiness driver is closed"));
            }
            let mut notified = 0;
            for _ in 0..if replay {
                self.0.batch.min(table.ready.queue.len())
            } else {
                0
            } {
                let token = table.ready.queue.pop_front().unwrap();
                table.ready.queue.push_back(token);
                if table.latch(token, &mut wakes) {
                    notified += 1;
                } else {
                    table.clear_ready(token);
                }
            }
            notified
        };
        let timeout = if notified == 0 {
            timeout
        } else {
            Some(Duration::ZERO)
        };
        let Driver { poll, events } = &mut *driver;
        // Tests observe admission after taking the driver lock and checking the
        // table, without competing for that lock or changing non-test builds.
        #[cfg(test)]
        {
            let before_poll = lock(&self.0.before_poll).take();
            if let Some(before_poll) = before_poll {
                before_poll();
            }
        }
        match poll.poll(events, timeout) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                return Ok(IoReport {
                    events: 0,
                    notified,
                });
            }
            Err(error) => return Err(os_error(error)),
        }
        let mut report = IoReport {
            notified,
            ..Default::default()
        };
        let mut table = lock(&self.0.table);
        if table.closed {
            return Err(failure("Readiness driver is closed"));
        }
        for event in events.iter() {
            report.events += 1;
            let route = table.routes.get(&event.token().0).copied();
            for direction in route.into_iter().flatten().flatten() {
                if (event.is_error()
                    || direction.interest.is_readable()
                        && (event.is_readable() || event.is_read_closed())
                    || direction.interest.is_writable()
                        && (event.is_writable() || event.is_write_closed()))
                    && table.latch(direction.token, &mut wakes)
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
        lock(&self.reactor.0.table).sockets -= 1;
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
    fn publish(&self, table: &mut Table) {
        if !table.closed {
            if let Some(token) = self.token {
                table.routes.insert(token, self.directions);
            }
        }
    }
    fn poison(&mut self, reactor: &Reactor, table: &mut Table, wakes: &mut Notifications) {
        self.poisoned = true;
        if let Some(token) = self.token.take() {
            table.routes.remove(&token);
            let _ = reactor.0.registry.deregister(&mut self.source);
        }
        // Remaining operations must observe the failure even without a new edge.
        for direction in self.directions.iter().flatten() {
            table.latch(direction.token, wakes);
        }
    }
}
/// Own a nonblocking Mio source. Active registrations retain the source until
/// synchronous deregistration, without borrowing language storage or raw fds.
pub struct IoSource<S>(Arc<Mutex<SourceState<S>>>);
impl<S: Source> IoSource<S> {
    pub(crate) fn allocation_size() -> usize {
        std::mem::size_of::<Mutex<SourceState<S>>>() + 2 * std::mem::size_of::<usize>()
    }
    pub fn new(source: S) -> Self {
        Self(Arc::new(Mutex::new(SourceState {
            source,
            token: None,
            directions: [None; 2],
            poisoned: false,
            reactor: None,
        })))
    }
    /// All I/O must go through the Mio source, including on Windows.
    pub fn with_io<T>(&self, operation: impl FnOnce(&mut S) -> T) -> T {
        operation(&mut lock(&self.0).source)
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
        let waker = if let Some(waker) = super::parallel::pool::io_waker() {
            Wake::Parallel(waker)
        } else {
            Wake::Local(
                super::executor::current_waker()
                    .ok_or_else(|| failure("I/O registration requires a task executor"))?,
            )
        };
        self.register_with(&reactor, interest, event, waker)
    }
    fn register_with(
        &self,
        reactor: &Reactor,
        interest: Interest,
        event: Option<ResourceEvent>,
        waker: impl Into<Wake>,
    ) -> Result<IoRegistration<S>, Error> {
        if event.as_ref().is_some_and(|e| !e.is_active()) {
            return Err(failure("Cannot register an inactive resource event"));
        }
        let mut wakes = Notifications::default();
        // Lock order is source -> table. Poll never locks a source. Holding the
        // table across the OS change and publication prevents a collected edge
        // from being discarded before its route/waiter exists.
        let mut state = lock(&self.0);
        let mut table = lock(&reactor.0.table);
        if table.closed {
            return Err(failure("Readiness driver is closed"));
        }
        if table.waiters.len() >= reactor.0.limit {
            return Err(Error::new_with_code(
                ErrorPhase::Runtime,
                ErrorCode::ResourceLimit,
                "Readiness registration limit exceeded",
            ));
        }
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
            .is_some_and(|owner| !Arc::ptr_eq(&owner.0, &reactor.0))
        {
            return Err(failure("I/O source belongs to another readiness driver"));
        }
        let token = table.next;
        let next = token
            .checked_add(1)
            .ok_or_else(|| failure("Readiness identities exhausted"))?;
        // Consume identities even when OS registration fails; never reuse them.
        table.next = next;
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
                state.poison(reactor, &mut table, &mut wakes);
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
        state.publish(&mut table);
        table.waiters.insert(
            token,
            Waiter {
                waker: waker.into(),
                event,
            },
        );
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
    source: Arc<Mutex<SourceState<S>>>,
    reactor: Reactor,
    token: usize,
    active: bool,
}
impl<S: Source> IoRegistration<S> {
    /// Whether the driver has collected an unconsumed readiness hint or closed.
    /// A closed driver wakes outstanding operations; rearm then reports closure.
    pub fn is_ready(&self) -> bool {
        let table = lock(&self.reactor.0.table);
        self.active && (table.closed || table.ready.tokens.contains(&self.token))
    }
    /// Rearm after WouldBlock or update the operation's readiness interest.
    pub fn rearm(&self, interest: Interest) -> Result<(), Error> {
        if !self.active {
            return Err(failure("Readiness registration has stopped"));
        }
        let mut wakes = Notifications::default();
        let mut state = lock(&self.source);
        let mut table = lock(&self.reactor.0.table);
        if table.closed {
            return Err(failure("Readiness driver is closed"));
        }
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
        table.clear_ready(self.token);
        if let Err(error) =
            self.reactor
                .0
                .registry
                .reregister(&mut state.source, Token(kernel), combined)
        {
            state.poison(&self.reactor, &mut table, &mut wakes);
            return Err(os_error(error));
        }
        state.directions[slot] = Some(Direction {
            token: self.token,
            interest,
        });
        state.publish(&mut table);
        Ok(())
    }
    /// Explicit synchronous stop with error reporting. Drop also stops; a failed
    /// deregistration poisons the source, and its old token remains invalid.
    pub fn stop(&mut self) -> Result<(), Error> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        let mut wakes = Notifications::default();
        let mut state = lock(&self.source);
        let mut table = lock(&self.reactor.0.table);
        table.waiters.remove(&self.token);
        table.clear_ready(self.token);
        for direction in &mut state.directions {
            if direction.is_some_and(|d| d.token == self.token) {
                *direction = None;
            }
        }
        if state.poisoned {
            return Err(failure("I/O source cannot be reused"));
        }
        let Some(kernel) = state.token else {
            return Ok(());
        };
        let result = if let Some(interest) = state.interest().filter(|_| !table.closed) {
            self.reactor
                .0
                .registry
                .reregister(&mut state.source, Token(kernel), interest)
        } else {
            table.routes.remove(&kernel);
            state.token = None;
            self.reactor.0.registry.deregister(&mut state.source)
        };
        if result.is_err() {
            state.poison(&self.reactor, &mut table, &mut wakes);
        } else {
            state.publish(&mut table);
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
