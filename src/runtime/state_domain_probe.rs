//! SC0 storage and ordering experiment, not a public domain implementation.
//! Roots use real arena/buffer accounting. Control events and queue admission are
//! deliberately a single-domain model; generated-code coverage lives separately.
use super::{
    arena::{Arena, Budget, ExternalCharge, LIMIT, LIVE, TRANSFER_FAIL},
    buffer::Buffer,
};
use std::{cell::Cell, ptr::NonNull, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Committed(u64),
    Conflict,
    Closed,
    Cancelled,
    Expired,
    ResourceLimit,
    Stale,
    Busy,
}

// This is a storage experiment layout, not a handwritten Joky ABI record.
struct Root {
    arena: Arena,
    value: NonNull<i32>,
    text: NonNull<Buffer>,
    releases: Rc<Cell<usize>>,
}

impl Root {
    fn new(
        budget: &Rc<Budget>,
        value: i32,
        text: &[u8],
        releases: &Rc<Cell<usize>>,
    ) -> Result<Self, crate::Error> {
        let mut arena = Arena::with_budget(budget.clone(), None);
        let pointer = arena
            .allocate(size_of::<i32>(), align_of::<i32>())?
            .cast::<i32>();
        let text = Buffer::copy_bytes(&mut arena, text)?;
        // SAFETY: A fresh, aligned allocation belongs exclusively to this root.
        unsafe { pointer.as_ptr().write(value) };
        Ok(Self {
            arena,
            value: pointer,
            text,
            releases: releases.clone(),
        })
    }

    fn value(&self) -> i32 {
        // SAFETY: The initialized scalar remains owned by this live root.
        unsafe { self.value.as_ptr().read() }
    }

    fn text(&self) -> &[u8] {
        // SAFETY: The buffer has plain byte elements and remains in this arena.
        unsafe { self.text.as_ref().bytes().unwrap() }
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        // SAFETY: No borrowed state escapes the probe. These are the unique
        // owners; no resource leaf, element descriptor or source Drop is present.
        unsafe {
            Buffer::release(&mut self.arena, self.text.as_ptr()).unwrap();
            self.arena.release(self.value.cast());
        }
        assert_eq!(self.arena.bytes(), 0);
        self.releases.set(self.releases.get() + 1);
    }
}

#[derive(Clone, Copy)]
struct Version {
    identity: u64,
    revision: u64,
}

struct Proposal {
    base: Version,
    candidate: Root,
    deadline: u64,
}

struct Receipt {
    outcome: Cell<Option<Outcome>>,
    _charge: ExternalCharge,
}

struct Request {
    proposal: Option<Proposal>,
    receipt: Rc<Receipt>,
}

impl Request {
    fn new(proposal: Proposal, records: &mut Arena) -> Result<Self, crate::Error> {
        let charge = records.reserve_external(size_of::<Receipt>())?;
        Ok(Self {
            proposal: Some(proposal),
            receipt: Rc::new(Receipt {
                outcome: Cell::new(None),
                _charge: charge,
            }),
        })
    }

    fn finish(&mut self, outcome: Outcome) {
        if self.receipt.outcome.get().is_none() {
            self.receipt.outcome.set(Some(outcome));
            self.proposal.take();
        }
    }
}

#[derive(Default)]
struct Control {
    cancelled: bool,
    now: u64,
}

struct Domain {
    identity: u64,
    revision: u64,
    root: Option<Root>,
    busy: bool,
    closing: bool,
    limit: usize,
}

impl Domain {
    fn new(root: Root) -> Self {
        Self {
            identity: 1,
            revision: 0,
            root: Some(root),
            busy: false,
            closing: false,
            limit: usize::MAX,
        }
    }

    fn version(&self) -> Version {
        Version {
            identity: self.identity,
            revision: self.revision,
        }
    }

    fn read(&self) -> Result<i32, Outcome> {
        if self.closing {
            Err(Outcome::Closed)
        } else if self.busy {
            Err(Outcome::Busy)
        } else {
            Ok(self.root.as_ref().unwrap().value())
        }
    }

    fn begin_write(&mut self) -> Result<u64, Outcome> {
        self.read()?;
        let next = self.revision.checked_add(1).ok_or(Outcome::ResourceLimit)?;
        self.busy = true;
        Ok(next)
    }

    fn end_write(&mut self, next: u64) {
        self.revision = next;
        self.release_access();
    }

    fn close(&mut self) {
        self.closing = true;
        if !self.busy {
            self.root.take();
        }
    }

    fn release_access(&mut self) {
        self.busy = false;
        if self.closing {
            self.root.take();
        }
    }

    fn validate(&self, proposal: &Proposal, control: &Control) -> Result<(), Outcome> {
        if self.closing {
            Err(Outcome::Closed)
        } else if proposal.base.identity != self.identity {
            Err(Outcome::Stale)
        } else if control.cancelled {
            Err(Outcome::Cancelled)
        } else if control.now >= proposal.deadline {
            Err(Outcome::Expired)
        } else if proposal.base.revision != self.revision {
            Err(Outcome::Conflict)
        } else {
            Ok(())
        }
    }

    // The injected boundary represents control events observed after preflight.
    // It runs before publication; publication has no callback parameter.
    fn commit(
        &mut self,
        request: &mut Request,
        control: &mut Control,
        fault: Option<usize>,
        after_preflight: impl FnOnce(&mut Self, &mut Control),
    ) -> Option<Root> {
        if request.receipt.outcome.get().is_some() {
            return None;
        }
        let proposal = request.proposal.take().unwrap();
        let prepare = || {
            self.validate(&proposal, control)?;
            if self.busy {
                return Err(Outcome::Busy);
            }
            checkpoint(fault, 0)?;
            let next = self.revision.checked_add(1).ok_or(Outcome::ResourceLimit)?;
            let root = self.root.as_ref().unwrap();
            if proposal.candidate.arena.bytes() > self.limit
                || proposal.candidate.arena.budget_pointer() != root.arena.budget_pointer()
            {
                return Err(Outcome::ResourceLimit);
            }
            checkpoint(fault, 1)?;
            // Candidate storage and the independent receipt were allocated before
            // admission. No arena registration/descriptor is built at publication.
            checkpoint(fault, 2)?;
            Ok(next)
        };
        let next = match prepare() {
            Ok(next) => next,
            Err(outcome) => {
                request.finish(outcome);
                return None;
            }
        };
        self.busy = true;
        after_preflight(self, control);
        if let Err(outcome) = self.validate(&proposal, control) {
            self.release_access();
            request.finish(outcome);
            return None;
        }
        let retired = self.publish(proposal.candidate, next, &request.receipt);
        self.release_access();
        retired
    }

    fn publish(&mut self, candidate: Root, next: u64, receipt: &Receipt) -> Option<Root> {
        // Only moves and scalar stores. Return the old owner rather than running
        // its cleanup in the publication interval. The arena moves with its
        // pinned accounting owners, preserving Buffer's nonowning budget pointers.
        let retired = self.root.replace(candidate);
        self.revision = next;
        receipt.outcome.set(Some(Outcome::Committed(next)));
        retired
    }
}

fn checkpoint(fault: Option<usize>, step: usize) -> Result<(), Outcome> {
    if fault == Some(step) {
        Err(Outcome::ResourceLimit)
    } else {
        Ok(())
    }
}

struct Fixture {
    domain: Domain,
    records: Arena,
    budget: Rc<Budget>,
    releases: Rc<Cell<usize>>,
}

impl Fixture {
    fn new() -> Self {
        assert_eq!(LIVE.with(Cell::get), 0);
        let budget = Rc::new(Budget {
            limit: Cell::new(64 * 1024),
            ..Default::default()
        });
        let releases = Rc::new(Cell::new(0));
        Self {
            domain: Domain::new(Root::new(&budget, 7, b"old", &releases).unwrap()),
            records: Arena::with_budget(budget.clone(), None),
            budget,
            releases,
        }
    }

    fn request(&mut self) -> Request {
        let proposal = Proposal {
            base: self.domain.version(),
            candidate: Root::new(&self.budget, 42, b"candidate", &self.releases).unwrap(),
            deadline: 10,
        };
        Request::new(proposal, &mut self.records).unwrap()
    }

    fn finish(mut self) {
        self.domain.close();
        assert_eq!(self.budget.bytes.get(), 0);
        assert_eq!(LIVE.with(Cell::get), 0);
    }
}

#[test]
fn sc0_probe_short_access_keeps_business_error_writes_and_defers_close() {
    let mut fixture = Fixture::new();
    let next = fixture.domain.begin_write().unwrap();
    assert_eq!(fixture.domain.begin_write(), Err(Outcome::Busy));
    assert_eq!(fixture.domain.read(), Err(Outcome::Busy));
    let business_result: Result<(), ()> = {
        // SAFETY: The probe holds this root's exclusive access until end_write.
        unsafe {
            fixture
                .domain
                .root
                .as_ref()
                .unwrap()
                .value
                .as_ptr()
                .write(9)
        };
        Err(())
    };
    assert!(business_result.is_err());
    fixture.domain.end_write(next);
    assert_eq!(fixture.domain.read(), Ok(9));
    assert_eq!(fixture.domain.revision, 1);

    fixture.domain.revision = u64::MAX;
    assert_eq!(fixture.domain.begin_write(), Err(Outcome::ResourceLimit));
    assert_eq!(fixture.domain.read(), Ok(9));
    fixture.domain.revision = 1;
    let next = fixture.domain.begin_write().unwrap();
    fixture.domain.close();
    assert_eq!(fixture.releases.get(), 0);
    assert_eq!(fixture.domain.read(), Err(Outcome::Closed));
    fixture.domain.end_write(next);
    assert_eq!(fixture.releases.get(), 1);
    fixture.finish();
}

#[test]
fn sc0_probe_every_preflight_failure_preserves_root_and_refunds_candidate() {
    for case in 0..7 {
        let mut fixture = Fixture::new();
        let baseline = fixture.budget.bytes.get();
        let old = fixture.domain.root.as_ref().unwrap().value;
        let mut request = fixture.request();
        match case {
            3 => fixture.domain.limit = 0,
            4 => request.proposal.as_mut().unwrap().base.revision = 99,
            5 => request.proposal.as_mut().unwrap().base.identity = 99,
            6 => fixture.domain.revision = u64::MAX,
            _ => {}
        }
        if case == 6 {
            request.proposal.as_mut().unwrap().base.revision = u64::MAX;
        }
        let revision = fixture.domain.revision;
        let retired = fixture.domain.commit(
            &mut request,
            &mut Control::default(),
            (case < 3).then_some(case),
            |_, _| panic!("failed preparation must not reach publication"),
        );
        assert!(retired.is_none());
        let expected = match case {
            4 => Outcome::Conflict,
            5 => Outcome::Stale,
            _ => Outcome::ResourceLimit,
        };
        assert_eq!(request.receipt.outcome.get(), Some(expected));
        assert_eq!(fixture.domain.revision, revision);
        assert_eq!(fixture.domain.root.as_ref().unwrap().value, old);
        assert_eq!(fixture.domain.root.as_ref().unwrap().text(), b"old");
        assert_eq!(fixture.domain.read(), Ok(7));
        assert_eq!(fixture.releases.get(), 1);
        drop(request);
        assert_eq!(fixture.budget.bytes.get(), baseline);
        fixture.finish();
    }
}

struct AllocationLimit(usize);
impl AllocationLimit {
    fn deny() -> Self {
        Self(LIMIT.with(|limit| limit.replace(0)))
    }
}
impl Drop for AllocationLimit {
    fn drop(&mut self) {
        LIMIT.with(|limit| limit.set(self.0));
    }
}

#[test]
fn sc0_probe_publication_moves_owners_without_allocating_or_running_cleanup() {
    let mut fixture = Fixture::new();
    let mut request = fixture.request();
    let candidate = request.proposal.as_ref().unwrap().candidate.value;
    let receipt = request.receipt.clone();
    let peak = fixture.budget.bytes.get();
    let retired = {
        let _deny = AllocationLimit::deny();
        fixture
            .domain
            .commit(&mut request, &mut Control::default(), None, |domain, _| {
                assert_eq!(domain.read(), Err(Outcome::Busy));
            })
    };
    assert_eq!(fixture.budget.bytes.get(), peak);
    assert_eq!(fixture.releases.get(), 0);
    assert_eq!(fixture.domain.root.as_ref().unwrap().value, candidate);
    assert_eq!(fixture.domain.root.as_ref().unwrap().text(), b"candidate");
    assert_eq!(fixture.domain.read(), Ok(42));
    assert_eq!(receipt.outcome.get(), Some(Outcome::Committed(1)));
    request.finish(Outcome::Cancelled);
    drop(request);
    assert_eq!(receipt.outcome.get(), Some(Outcome::Committed(1)));
    drop(retired);
    assert_eq!(fixture.releases.get(), 1);
    fixture.domain.close();
    assert_eq!(fixture.releases.get(), 2);
    assert_eq!(fixture.budget.bytes.get(), size_of::<Receipt>());
    drop(receipt);
    fixture.finish();
}

#[test]
fn sc0_probe_final_gate_observes_late_cancellation_close_and_clock_expiry() {
    for outcome in [Outcome::Cancelled, Outcome::Closed, Outcome::Expired] {
        let mut fixture = Fixture::new();
        let mut request = fixture.request();
        let retired = fixture.domain.commit(
            &mut request,
            &mut Control::default(),
            None,
            |domain, control| match outcome {
                Outcome::Cancelled => control.cancelled = true,
                Outcome::Closed => {
                    domain.close();
                    assert!(domain.root.is_some());
                }
                Outcome::Expired => control.now = 10,
                _ => unreachable!(),
            },
        );
        assert!(retired.is_none());
        assert_eq!(request.receipt.outcome.get(), Some(outcome));
        assert_eq!(fixture.domain.revision, 0);
        if outcome != Outcome::Closed {
            assert_eq!(fixture.domain.read(), Ok(7));
        }
        drop(request);
        fixture.finish();
    }
}

#[test]
fn sc0_probe_all_cancel_close_publish_orders_have_one_terminal_outcome() {
    // Independent oracle: whichever terminal action is accepted first wins.
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let mut fixture = Fixture::new();
        let mut request = fixture.request();
        let receipt = request.receipt.clone();
        let expected = [Outcome::Committed(1), Outcome::Cancelled, Outcome::Closed][order[0]];
        for action in order {
            match action {
                0 => drop(fixture.domain.commit(
                    &mut request,
                    &mut Control::default(),
                    None,
                    |_, _| {},
                )),
                1 => request.finish(Outcome::Cancelled),
                2 => {
                    fixture.domain.close();
                    request.finish(Outcome::Closed);
                }
                _ => unreachable!(),
            }
            assert_eq!(receipt.outcome.get(), Some(expected));
        }
        assert_eq!(fixture.releases.get(), 2);
        drop(request);
        assert_eq!(receipt.outcome.get(), Some(expected));
        drop(receipt);
        fixture.finish();
    }
}

#[test]
fn sc0_probe_peak_and_receipt_admission_fail_before_changing_state() {
    let mut fixture = Fixture::new();
    let baseline = fixture.budget.bytes.get();
    // Fail at each byte of candidate/receipt preparation, including partial
    // Buffer allocation. The persistent root must remain independently usable.
    let full = {
        let request = fixture.request();
        let full = fixture.budget.bytes.get();
        drop(request);
        full
    };
    for limit in baseline..full {
        fixture.budget.limit.set(limit);
        let candidate = Root::new(&fixture.budget, 42, b"candidate", &fixture.releases);
        if let Ok(candidate) = candidate {
            let proposal = Proposal {
                base: fixture.domain.version(),
                candidate,
                deadline: 10,
            };
            assert!(Request::new(proposal, &mut fixture.records).is_err());
        }
        assert_eq!(fixture.budget.bytes.get(), baseline, "limit {limit}");
        assert_eq!(fixture.domain.read(), Ok(7));
        assert_eq!(fixture.domain.revision, 0);
    }
    fixture.budget.limit.set(full);
    let request = fixture.request();
    assert_eq!(fixture.budget.bytes.get(), full);
    drop(request);
    fixture.budget.limit.set(64 * 1024);
    for cycle in 0..64 {
        let mut request = fixture.request();
        if cycle % 2 == 0 {
            drop(
                fixture
                    .domain
                    .commit(&mut request, &mut Control::default(), None, |_, _| {}),
            );
        } else {
            request.finish(Outcome::Cancelled);
        }
        drop(request);
        assert_eq!(fixture.records.bytes(), 0);
        assert_eq!(
            fixture.budget.bytes.get(),
            fixture.domain.root.as_ref().unwrap().arena.bytes()
        );
    }
    fixture.finish();
}

struct TransferFault;
impl Drop for TransferFault {
    fn drop(&mut self) {
        TRANSFER_FAIL.with(|fault| fault.set(None));
    }
}

#[test]
fn sc0_probe_existing_graph_transfer_isolates_candidate_from_task_temporaries() {
    for fault in [Some(0), Some(1), Some(2), None] {
        let fixture = Fixture::new();
        let baseline = fixture.budget.bytes.get();
        let mut candidate =
            Root::new(&fixture.budget, 42, b"candidate", &fixture.releases).unwrap();
        let temporary = candidate.arena.allocate(31, 8).unwrap();
        let original = candidate.value;
        let peak = fixture.budget.bytes.get();
        // SAFETY: These are unique initialized byte-buffer roots in this arena;
        // discovery neither invokes callbacks nor retains a borrowed view.
        let mut graph = unsafe {
            Buffer::owning_allocations(&candidate.arena, [candidate.text.as_ptr()]).unwrap()
        };
        graph.allocations.push(candidate.value.cast());
        assert_eq!(graph.metadata, 0);
        let mut capsule = Arena::with_budget(fixture.budget.clone(), None);
        let _reset = TransferFault;
        TRANSFER_FAIL.with(|slot| slot.set(fault));
        let result = candidate
            .arena
            .transfer(&mut capsule, &graph.allocations, usize::MAX);
        assert_eq!(fixture.budget.bytes.get(), peak);
        assert!(candidate.arena.contains(temporary));
        if fault.is_some() {
            assert!(result.is_err());
            assert_eq!(capsule.bytes(), 0);
            assert!(graph
                .allocations
                .iter()
                .all(|p| candidate.arena.contains(*p)));
            // SAFETY: The temporary has no references or owning descriptor.
            unsafe { candidate.arena.release(temporary) };
        } else {
            result.unwrap();
            // SAFETY: The complete graph is now registered with capsule, sharing
            // the same peak budget. This nonfallible metadata migration must
            // follow transfer before the source account can be dropped.
            unsafe { Buffer::migrate_metadata(&graph.buffers, &capsule) };
            let task = std::mem::replace(&mut candidate.arena, capsule);
            assert!(task.contains(temporary));
            assert!(!candidate.arena.contains(temporary));
            drop(task);
            assert_eq!(candidate.value, original);
        }
        assert_eq!(candidate.value(), 42);
        assert_eq!(candidate.text(), b"candidate");
        assert_eq!(fixture.domain.read(), Ok(7));
        drop(candidate);
        assert_eq!(fixture.budget.bytes.get(), baseline);
        fixture.finish();
    }
}

#[test]
fn sc0_probe_existing_instances_outlive_independent_candidate_tasks() {
    use crate::vm::{InstanceValue, VmEngine, VmMode};
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!("../../tests/fixtures/state_domain_probe.jk"))
            .unwrap_or_else(|error| panic!("{error}"));
        let state = vm.create_instance("initial", &[]).unwrap();
        let live = LIVE.with(Cell::get);
        let bytes = vm.instance_bytes();
        let task = vm.call_async("pending_candidate", &[]).unwrap();
        assert!(!task.is_finished());
        assert!(LIVE.with(Cell::get) > live);
        drop(task);
        assert_eq!(LIVE.with(Cell::get), live);
        assert_eq!(vm.instance_bytes(), bytes);
        assert_eq!(
            vm.call_instance(state, "state_count", &[]).unwrap(),
            InstanceValue::Int32(1)
        );
        let mut task = vm.call_async("replacement", &[]).unwrap();
        while !task.is_finished() {
            task.resume().unwrap();
        }
        assert_eq!(task.cps_result(), Some(42));
        drop(task);
        assert_eq!(LIVE.with(Cell::get), live);
        vm.close_instance(state).unwrap();
        assert_eq!(vm.instance_bytes(), 0);
        drop(vm);
        assert_eq!(LIVE.with(Cell::get), 0);
    }
}
